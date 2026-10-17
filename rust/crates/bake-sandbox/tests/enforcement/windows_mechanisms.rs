//! A mechanism probe, not a qualification: real Windows effects under the
//! retained `WRITE_RESTRICTED` token and two candidate replacements, so the
//! roadmap's Windows confinement decision rests on measured behaviour.
//!
//! - **`write-restricted`** is the shipped runner, reached through the same
//!   explicit chain as the other scenarios in [`super`].
//! - **`low-integrity`** is that token built here the same way, then lowered
//!   to the low integrity level (`S-1-16-4096`) before `CreateProcessAsUserW`.
//!   Its workspace and a second, "foreign" workspace carry an inheritable
//!   low label, as a low-integrity design would need.
//! - **`app-container`** is a process started with
//!   `PROC_THREAD_ATTRIBUTE_SECURITY_CAPABILITIES` for a profile this test
//!   creates and deletes, whose package SID is granted on its workspace.
//!
//! Each candidate attempts the same effects, one primitive per run, on roots
//! whose DACL grants this user full access ([`super::user_scratch`]), so CI's
//! Administrators membership cannot produce a denial. Outcomes are decided
//! by the disk as this unconfined process sees it, never by the probe's exit
//! status alone: a run that reports success must show the effect, and one
//! that reports a denial must leave the disk unchanged, or the case panics.
//! Any other failure, including a candidate that cannot start, panics with
//! its evidence: nothing here passes by skipping.
//!
//! The direction of each effect is printed, not asserted, as one
//! `mechanism-probe:` line with the prediction beside it. The predictions
//! come from Microsoft's documentation and are only predictions until a
//! Windows build has run this; Linux compiles none of it. What the test does
//! assert beyond consistency is its own hygiene: every candidate process,
//! descendants included, is born in a Job this test owns and is reaped
//! before its roots, labels, ACEs, and profile are removed, every capability
//! and package grant and low label is read back from the root's descriptor
//! as exactly the ACE intended, a descendant
//! started with `CREATE_BREAKAWAY_FROM_JOB` is refused, and the fixture's
//! parent-route deletion works unconfined (the control that makes the
//! confined result meaningful).
//!
//! `cargo test --locked -p bake-sandbox --test enforcement windows-mechanism`
//! runs these three tests; `grep '^mechanism-probe:'` on its output extracts
//! the evidence, the first line of each test naming the Windows build.

use std::ffi::{OsStr, c_void};
use std::io::Read;
use std::os::windows::ffi::OsStrExt;
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::ptr::{null, null_mut};

use bake_sandbox::backends::windows_acl::{GRANT_MASK, workspace_write_sid};

use super::{
    BOUND, DENIED, PROBE_ARG, Scratch, TestJob, confined, exe, harness, signaled_within, text,
    unconfined, user_scratch, win32, ww,
};

// ---------------------------------------------------------------------------
// The confined side: operations only this probe needs.

/// The probe operations behind `PROBE_ARG mechanism ...`, with the same
/// exit convention as [`super::probe_main`].
pub fn probe(ops: &[&str]) -> std::io::Result<i32> {
    match ops {
        // The integrity level and AppContainer flag of this process's token.
        // TEMP, TMP, and LOCALAPPDATA as this process sees them, one per
        // line: Windows reroutes all three for a profile.
        ["env-pair"] => {
            for name in ["TEMP", "TMP", "LOCALAPPDATA"] {
                println!("{}", std::env::var(name).unwrap_or_default());
            }
            Ok(0)
        }
        ["identity"] => identity().map(|line| {
            println!("{line}");
            0
        }),
        // A TCP connection to a listener the test owns, bounded.
        ["connect", address] => {
            let address: std::net::SocketAddr = address.parse().expect("socket address");
            std::net::TcpStream::connect_timeout(&address, std::time::Duration::from_secs(5))
                .map(|_| 0)
        }
        // A parked descendant that inherits this process's standard
        // handles, its output files (no `NUL` open, which a container may
        // lack), left running.
        ["orphan"] => std::process::Command::new(std::env::current_exe()?)
            .args([PROBE_ARG, "park"])
            .spawn()
            .map(|child| {
                println!("grandchild {}", child.id());
                0
            }),
        // The same parked descendant, then this process parks too, after a
        // handshake naming both: a confined tree for a runner to lose.
        ["orphan-and-park"] => {
            use std::io::Write;
            let child = std::process::Command::new(std::env::current_exe()?)
                .args([PROBE_ARG, "park"])
                .spawn()?;
            println!("grandchild {} child {}", child.id(), std::process::id());
            std::io::stdout().flush()?;
            loop {
                std::thread::park();
            }
        }
        // A descendant that asks to leave the Job. Should Windows allow it,
        // the descendant is killed and reaped before this reports, so no
        // process outlives the run either way.
        ["breakaway"] => {
            use std::os::windows::process::CommandExt;
            use windows_sys::Win32::System::Threading::CREATE_BREAKAWAY_FROM_JOB;
            let mut child = std::process::Command::new(std::env::current_exe()?)
                .args([PROBE_ARG, "park"])
                .creation_flags(CREATE_BREAKAWAY_FROM_JOB)
                .spawn()?;
            let _ = child.kill();
            let _ = child.wait();
            println!("escaped {}", child.id());
            Ok(0)
        }
        other => panic!("unknown mechanism operation {other:?}"),
    }
}

fn identity() -> std::io::Result<String> {
    use windows_sys::Win32::Security::{
        GetTokenInformation, TOKEN_MANDATORY_LABEL, TOKEN_QUERY, TokenIntegrityLevel,
        TokenIsAppContainer,
    };
    use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};
    let mut raw = null_mut();
    // SAFETY: the pseudo-handle needs no closing.
    check(
        unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut raw) },
        "OpenProcessToken",
    )?;
    // SAFETY: a fresh handle owned here.
    let token = unsafe { OwnedHandle::from_raw_handle(raw) };
    let label = token_information(&token, TokenIntegrityLevel)?;
    // SAFETY: the buffer holds a TOKEN_MANDATORY_LABEL.
    let level =
        win32::sid_text(unsafe { (*label.as_ptr().cast::<TOKEN_MANDATORY_LABEL>()).Label.Sid });
    let mut container = 0u32;
    let mut size = 0u32;
    // SAFETY: `container` has the size passed.
    check(
        unsafe {
            GetTokenInformation(
                token.as_raw_handle(),
                TokenIsAppContainer,
                std::ptr::addr_of_mut!(container).cast(),
                size_of::<u32>() as u32,
                &mut size,
            )
        },
        "GetTokenInformation(TokenIsAppContainer)",
    )?;
    Ok(format!("integrity={level} app_container={container}"))
}

// ---------------------------------------------------------------------------
// Harness glue.

pub fn tests() -> Vec<harness::Test> {
    let scenarios: &[(&str, fn())] = &[
        ("write_restricted", write_restricted),
        ("low_integrity", low_integrity),
        ("app_container", app_container),
    ];
    scenarios
        .iter()
        .map(|(name, scenario)| {
            let scenario = *scenario;
            harness::Test::new(format!("windows-mechanism::{name}"), scenario)
        })
        .collect()
}

/// `FILE_GENERIC_READ | FILE_GENERIC_EXECUTE` (0x1200A9), `icacls`'s `RX`:
/// what the AppContainer's package SID needs to read and run its binary.
pub(super) const READ_EXECUTE: u32 = 0x0012_00A9;

/// The package SID's grant on its own workspace: [`READ_EXECUTE`] plus the
/// runner's write grant (0x1301FF), still without `WRITE_DAC` and
/// `WRITE_OWNER`.
pub(super) const CONTAINER_WORKSPACE: u32 = READ_EXECUTE | GRANT_MASK;

// The runner's write grant, `GENERIC_WRITE`'s file rights, `DELETE`,
// `FILE_DELETE_CHILD`, and `SYNCHRONIZE` (`icacls` `WD,AD,WEA,WA,D,DC,S`),
// and the container's workspace grant, pinned numerically.
const _: () = assert!(GRANT_MASK == 0x0011_0156);
const _: () = assert!(CONTAINER_WORKSPACE == 0x0013_01FF);

/// `FILE_ALL_ACCESS` without `DELETE`: a file that can be deleted only
/// through its parent's `FILE_DELETE_CHILD` (\[MS-FSA\] 2.1.5.1.2.1).
const ALL_BUT_DELETE: u32 = 0x001F_01FF & !0x0001_0000;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Candidate {
    WriteRestricted,
    LowIntegrity,
    AppContainer,
}

impl Candidate {
    fn name(self) -> &'static str {
        match self {
            Self::WriteRestricted => "write-restricted",
            Self::LowIntegrity => "low-integrity",
            Self::AppContainer => "app-container",
        }
    }
}

/// How a candidate's process is started.
enum Launch<'a> {
    /// The shipped runner, through the explicit chain.
    Runner,
    /// `CreateProcessAsUserW` with this primary token.
    Token(&'a OwnedHandle),
    /// `CreateProcessW` as this AppContainer, running `exe`.
    Container { sid: *mut c_void, exe: &'a Path },
}

/// One run's result, with the live members of its Job when its direct
/// process exited (empty for the runner, which owns its own Job).
pub(super) struct Outcome {
    pub(super) code: Option<i32>,
    pub(super) stdout: String,
    pub(super) stderr: String,
    left_in_job: Vec<u32>,
}

impl Outcome {
    fn reported(&self) -> Reported {
        match self.code {
            Some(0) => Reported::Ok,
            Some(DENIED) if self.stderr.contains("denied:") => Reported::Denied,
            _ => Reported::Other,
        }
    }

    fn evidence(&self) -> String {
        format!(
            "exit={:?} stdout={:?} stderr={:?}",
            self.code,
            self.stdout.trim(),
            self.stderr.trim()
        )
    }
}

#[derive(PartialEq, Eq)]
enum Reported {
    Ok,
    Denied,
    Other,
}

struct Probe<'a> {
    candidate: Candidate,
    launch: Launch<'a>,
    workspace: &'a Scratch,
}

impl Probe<'_> {
    fn run(&self, ops: &[&str]) -> Outcome {
        match &self.launch {
            Launch::Runner => {
                let run = confined(&ww(self.workspace), &self.workspace.0, ops);
                Outcome {
                    code: run.code,
                    stdout: run.stdout,
                    stderr: run.stderr,
                    left_in_job: Vec::new(),
                }
            }
            Launch::Token(token) => launch(
                Some(*token),
                None,
                Path::new(&exe()),
                ops,
                &self.workspace.0,
            ),
            Launch::Container { sid, exe } => launch(None, Some(*sid), exe, ops, &self.workspace.0),
        }
    }

    fn line(&self, fields: std::fmt::Arguments<'_>) {
        eprintln!(
            "mechanism-probe: candidate={} {fields}",
            self.candidate.name()
        );
    }

    /// Runs `ops`, which attempt one file effect, and decides it by the disk:
    /// `happened` reads the effect back from this unconfined process.
    fn effect(&self, case: &str, predicted: &str, ops: &[&str], happened: impl Fn() -> bool) {
        let outcome = self.run(ops);
        let observed = happened();
        let consistent = match outcome.reported() {
            Reported::Ok => observed,
            Reported::Denied => !observed,
            Reported::Other => panic!(
                "{} {case}: neither success nor a denial: {}",
                self.candidate.name(),
                outcome.evidence()
            ),
        };
        assert!(
            consistent,
            "{} {case}: the report contradicts the disk (effect on disk: {observed}): {}",
            self.candidate.name(),
            outcome.evidence()
        );
        let observed = if observed { "effect" } else { "denied" };
        self.line(format_args!(
            "case={case} predicted={predicted} observed={observed} matches={} exit={:?}",
            matches(predicted, observed),
            outcome.code
        ));
    }

    /// Runs `ops`, a compatibility check, and records what it did. A
    /// success whose output is not `expected` panics.
    fn compatibility(&self, case: &str, predicted: &str, ops: &[&str], expected: Option<&str>) {
        let outcome = self.run(ops);
        let observed = match outcome.reported() {
            Reported::Ok => {
                if let Some(expected) = expected {
                    assert_eq!(
                        outcome.stdout.trim(),
                        expected,
                        "{} {case}: {}",
                        self.candidate.name(),
                        outcome.evidence()
                    );
                }
                "allowed"
            }
            Reported::Denied => "denied",
            Reported::Other => "failed",
        };
        self.line(format_args!(
            "case={case} predicted={predicted} observed={observed} matches={} exit={:?} stderr={:?}",
            matches(predicted, observed),
            outcome.code,
            outcome.stderr.trim()
        ));
    }
}

/// Whether `observed` is what was predicted; `n/a` for an `unknown`
/// prediction, which the sources do not settle.
fn matches(predicted: &str, observed: &str) -> &'static str {
    match (predicted, predicted == observed) {
        ("unknown", _) => "n/a",
        (_, true) => "true",
        (_, false) => "false",
    }
}

/// The Windows build, as `ver` prints it (major, minor, build, and UBR).
fn build() -> String {
    let output = std::process::Command::new("cmd")
        .args(["/d", "/c", "ver"])
        .stdin(Stdio::null())
        .output()
        .expect("cmd /c ver");
    String::from_utf8_lossy(&output.stdout).trim().to_owned()
}

fn icacls(path: &Path, args: &[&str]) {
    let output = std::process::Command::new("icacls")
        .arg(path)
        .args(args)
        .arg("/q")
        .stdin(Stdio::null())
        .output()
        .expect("icacls");
    assert!(
        output.status.success(),
        "icacls {} {args:?}: {}\n{}{}",
        path.display(),
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

/// An inheritable low label with the default no-write-up policy. The scratch
/// root grants this user full access, `WRITE_OWNER` included, which setting
/// a label at or below this process's level needs. The label is read back:
/// exactly one `OI|CI` no-write-up label ACE for `S-1-16-4096`.
fn label_low(root: &Scratch) {
    icacls(&root.0, &["/setintegritylevel", "(OI)(CI)L"]);
    let label = acl::Descriptor::read(&root.0)
        .unwrap_or_else(|error| panic!("read back {}: {error}", root.0.display()))
        .label;
    let expected = acl::Ace {
        kind: acl::MANDATORY_LABEL,
        flags: acl::OI_CI,
        mask: acl::NO_WRITE_UP,
        sid: "S-1-16-4096".to_owned(),
    };
    assert_eq!(
        label,
        vec![expected],
        "the low label on {}",
        root.0.display()
    );
}

/// Grants `sid` exactly `mask` on `root`, inherited by files and directories
/// (`OI|CI`), and checks it by reading the descriptor back.
///
/// The ACE is merged with `SetEntriesInAclW` from a binary SID trustee and
/// applied with `SetNamedSecurityInfoW` and `DACL_SECURITY_INFORMATION`
/// alone, as the runner's `grant_write` does, so the owner and integrity
/// label are not written. The binary SID trustee avoids account-name
/// resolution for synthetic capability SIDs. The read-back panics
/// unless the DACL afterwards is the DACL before, every ACE kept (on a
/// [`user_scratch`] root, the full-access ACE that lets the scratch guard
/// clean up), plus exactly one explicit allow ACE for `sid` with `mask` and
/// `OI|CI`, and the label is unchanged.
fn grant(candidate: Candidate, root: &Scratch, sid: &str, mask: u32) {
    let trustee = token::Sid::parse(sid).unwrap_or_else(|error| panic!("SID {sid}: {error}"));
    let canonical = win32::sid_text(trustee.as_ptr());
    let read = |when: &str| {
        acl::Descriptor::read(&root.0)
            .unwrap_or_else(|error| panic!("read {} {when}: {error}", root.0.display()))
    };
    let before = read("before the grant");
    assert!(
        before.dacl.iter().all(|ace| ace.sid != canonical),
        "{} already names {canonical}: {:?}",
        root.0.display(),
        before.dacl
    );
    acl::grant(&root.0, &trustee, mask)
        .unwrap_or_else(|error| panic!("grant {canonical} on {}: {error}", root.0.display()));
    let after = read("after the grant");
    let mut added = after.dacl.clone();
    for ace in &before.dacl {
        let index = added
            .iter()
            .position(|kept| kept == ace)
            .unwrap_or_else(|| {
                panic!(
                    "the grant on {} dropped {ace:?}: after {:?}",
                    root.0.display(),
                    after.dacl
                )
            });
        added.swap_remove(index);
    }
    let expected = acl::Ace {
        kind: acl::ACCESS_ALLOWED,
        flags: acl::OI_CI,
        mask,
        sid: canonical.clone(),
    };
    assert_eq!(
        added,
        vec![expected],
        "the grant on {} must add exactly one ACE",
        root.0.display()
    );
    assert_eq!(
        after.label,
        before.label,
        "the grant on {} must keep its label",
        root.0.display()
    );
    eprintln!(
        "mechanism-probe: candidate={} fixture=grant root={} sid={canonical} mask={mask:#x} flags=OI|CI",
        candidate.name(),
        root.0.display()
    );
}

/// Every root a candidate's effects touch. Files are seeded after labels
/// and grants, so each inherits them at creation.
struct Roots {
    workspace: Scratch,
    outside: Scratch,
    /// Another workspace, labelled low and granted its own capability SID,
    /// as a second Bake workspace under a low-integrity design would be.
    foreign: Scratch,
}

impl Roots {
    fn new(candidate: Candidate) -> Self {
        let roots = Self {
            workspace: user_scratch("mech-ws"),
            outside: user_scratch("mech-out"),
            foreign: user_scratch("mech-foreign"),
        };
        label_low(&roots.foreign);
        grant(
            candidate,
            &roots.foreign,
            &workspace_write_sid(&text(&roots.foreign.0)),
            GRANT_MASK,
        );
        if candidate == Candidate::LowIntegrity {
            label_low(&roots.workspace);
            grant(
                candidate,
                &roots.workspace,
                &workspace_write_sid(&text(&roots.workspace.0)),
                GRANT_MASK,
            );
        }
        roots
    }
}

/// Seeds `name` under `root` with a DACL that grants this user everything
/// but `DELETE`, so only the parent's `FILE_DELETE_CHILD` can delete it.
pub(super) fn seed_without_delete(root: &Scratch, name: &str, user: &str) -> PathBuf {
    use windows_sys::Win32::Security::Authorization::{
        ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1,
    };
    use windows_sys::Win32::Security::{
        DACL_SECURITY_INFORMATION, PROTECTED_DACL_SECURITY_INFORMATION, SetFileSecurityW,
    };
    let path = root.write(name, "keep");
    let sddl = wide(OsStr::new(&format!("D:P(A;;{ALL_BUT_DELETE:#x};;;{user})")));
    let mut descriptor = null_mut();
    // SAFETY: NUL-terminated SDDL; the descriptor is LocalAlloc'd.
    let ok = unsafe {
        ConvertStringSecurityDescriptorToSecurityDescriptorW(
            sddl.as_ptr(),
            SDDL_REVISION_1,
            &mut descriptor,
            null_mut(),
        )
    };
    check(ok, "ConvertStringSecurityDescriptorToSecurityDescriptorW").expect("SDDL");
    let descriptor = Local(descriptor);
    let units = wide(path.as_os_str());
    // SAFETY: NUL-terminated path and a valid self-relative descriptor.
    let ok = unsafe {
        SetFileSecurityW(
            units.as_ptr(),
            DACL_SECURITY_INFORMATION | PROTECTED_DACL_SECURITY_INFORMATION,
            descriptor.0,
        )
    };
    check(ok, "SetFileSecurityW").expect("a DACL without DELETE");
    path
}

/// The effect and compatibility cases every candidate runs.
fn matrix(probe: &Probe<'_>, roots: &Roots, predictions: &Predictions) {
    let user = win32::user_sid().expect("this process's user SID");
    let (workspace, outside, foreign) = (&roots.workspace, &roots.outside, &roots.foreign);

    let victim = outside.write("victim-delete.txt", "keep");
    probe.effect(
        "outside-delete-file",
        predictions.outside_delete,
        &["win32-delete", "DeleteFileW", &text(&victim)],
        || !victim.exists(),
    );

    // The parent route: the control proves it works for this user here.
    let control = seed_without_delete(outside, "control-parent.txt", &user);
    eprintln!(
        "mechanism-probe: candidate={} fixture=parent-route file={}",
        probe.candidate.name(),
        win32::sddl(&control)
    );
    let output = unconfined(&["win32-delete", "DeleteFileW", &text(&control)]);
    assert!(
        output.status.success() && !control.exists(),
        "the parent-route control must delete unconfined: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let victim = seed_without_delete(outside, "victim-parent.txt", &user);
    probe.effect(
        "outside-delete-via-parent",
        predictions.outside_delete_via_parent,
        &["win32-delete", "DeleteFileW", &text(&victim)],
        || !victim.exists(),
    );

    let source = outside.write("victim-rename.txt", "keep");
    let destination = workspace.join("renamed-in.txt");
    probe.effect(
        "outside-rename-into-workspace",
        predictions.outside_rename,
        &["rename", &text(&source), &text(&destination)],
        || !source.exists() && destination.exists(),
    );

    let victim = foreign.write("victim.txt", "keep");
    probe.effect(
        "foreign-workspace-delete",
        predictions.foreign_delete,
        &["win32-delete", "DeleteFileW", &text(&victim)],
        || !victim.exists(),
    );

    let created = workspace.join("created.txt");
    probe.effect(
        "workspace-create",
        "effect",
        &["write", &text(&created), "made"],
        || std::fs::read_to_string(&created).is_ok_and(|text| text == "made"),
    );
    let seeded = workspace.write("seed.txt", "seed");
    probe.effect(
        "workspace-delete",
        "effect",
        &["win32-delete", "DeleteFileW", &text(&seeded)],
        || !seeded.exists(),
    );

    let readable = outside.write("readme.txt", "outside");
    probe.compatibility(
        "outside-read",
        predictions.outside_read,
        &["read", &text(&readable)],
        Some("outside"),
    );
    let readable = workspace.write("readme.txt", "inside");
    probe.compatibility(
        "workspace-read",
        "allowed",
        &["read", &text(&readable)],
        Some("inside"),
    );
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("a loopback listener");
    let address = listener.local_addr().expect("listener address").to_string();
    probe.compatibility(
        "loopback-connect",
        predictions.loopback,
        &["mechanism", "connect", &address],
        None,
    );
    drop(listener);
    probe.compatibility("spawn-child", "allowed", &["spawn", "exit", "0"], None);
}

/// Predictions from Microsoft's documentation and, for the shipped token,
/// the measured 10.0.26300 record; `unknown` where the sources conflict.
struct Predictions {
    outside_delete: &'static str,
    outside_delete_via_parent: &'static str,
    outside_rename: &'static str,
    foreign_delete: &'static str,
    outside_read: &'static str,
    loopback: &'static str,
}

/// Asserts the lifecycle a candidate's process tree must keep: a descendant
/// left running is still in the Job when its parent exits (and
/// [`TestJob::finish`] reaps it), and a request to break away is refused.
fn lifecycle(probe: &Probe<'_>) {
    let outcome = probe.run(&["mechanism", "orphan"]);
    assert!(
        outcome.reported() == Reported::Ok,
        "{} orphan: {}",
        probe.candidate.name(),
        outcome.evidence()
    );
    let grandchild: u32 = outcome
        .stdout
        .trim()
        .strip_prefix("grandchild ")
        .and_then(|pid| pid.parse().ok())
        .unwrap_or_else(|| panic!("grandchild pid in {}", outcome.evidence()));
    assert!(
        outcome.left_in_job.contains(&grandchild),
        "{} orphan: grandchild {grandchild} was not in the Job: {:?}",
        probe.candidate.name(),
        outcome.left_in_job
    );
    probe.line(format_args!(
        "case=descendant-in-job observed=contained grandchild={grandchild}"
    ));
    let outcome = probe.run(&["mechanism", "breakaway"]);
    assert!(
        outcome.reported() == Reported::Denied,
        "{} breakaway must be refused: {}",
        probe.candidate.name(),
        outcome.evidence()
    );
    probe.line(format_args!("case=breakaway observed=denied"));
}

// ---------------------------------------------------------------------------
// Scenarios.

fn write_restricted() {
    let roots = Roots::new(Candidate::WriteRestricted);
    let probe = Probe {
        candidate: Candidate::WriteRestricted,
        launch: Launch::Runner,
        workspace: &roots.workspace,
    };
    probe.line(format_args!("build={:?}", build()));
    let token = probe.run(&["token"]);
    probe.line(format_args!("token={:?}", token.stdout.trim()));
    // The runner's own lifecycle is asserted by the scenarios in `super`.
    matrix(
        &probe,
        &roots,
        &Predictions {
            outside_delete: "effect",
            outside_delete_via_parent: "effect",
            outside_rename: "effect",
            foreign_delete: "effect",
            outside_read: "allowed",
            loopback: "allowed",
        },
    );
}

fn low_integrity() {
    let roots = Roots::new(Candidate::LowIntegrity);
    let capability = workspace_write_sid(&text(&roots.workspace.0));
    let token = token::low_integrity(&capability).expect("the low-integrity token");
    let probe = Probe {
        candidate: Candidate::LowIntegrity,
        launch: Launch::Token(&token),
        workspace: &roots.workspace,
    };
    probe.line(format_args!("build={:?}", build()));
    let identity = probe.run(&["mechanism", "identity"]);
    assert!(
        identity.reported() == Reported::Ok && identity.stdout.contains("integrity=S-1-16-4096"),
        "the candidate must run at low integrity: {}",
        identity.evidence()
    );
    let restricted = probe.run(&["token"]);
    assert!(
        restricted.stdout.contains("restricted=true"),
        "the candidate must be write-restricted: {}",
        restricted.evidence()
    );
    probe.line(format_args!(
        "identity={:?} token={:?}",
        identity.stdout.trim(),
        restricted.stdout.trim()
    ));
    matrix(
        &probe,
        &roots,
        &Predictions {
            // No-write-up is defined over the type's generic-write mapping,
            // which omits both deletion rights (MS7, MS8), yet MS7 also says
            // a low process "cannot modify or delete" a medium file.
            outside_delete: "unknown",
            outside_delete_via_parent: "unknown",
            outside_rename: "unknown",
            // A low object passes the label check, and the restricting SIDs
            // are not consulted for DELETE.
            foreign_delete: "effect",
            outside_read: "allowed",
            loopback: "allowed",
        },
    );
    lifecycle(&probe);
}

fn app_container() {
    // Dropped last: every process is reaped and every root removed first.
    let profile = Profile::create();
    let roots = Roots::new(Candidate::AppContainer);
    grant(
        Candidate::AppContainer,
        &roots.workspace,
        &profile.sid_text,
        CONTAINER_WORKSPACE,
    );
    // The container cannot read Cargo's target directory, so it runs a copy
    // of this binary from a root that grants it read and execute.
    let bin = Scratch::outside("mech-bin");
    grant(
        Candidate::AppContainer,
        &bin,
        &profile.sid_text,
        READ_EXECUTE,
    );
    let copy = bin.join("probe.exe");
    std::fs::copy(std::env::current_exe().expect("exe"), &copy).expect("copy the test binary");
    let probe = Probe {
        candidate: Candidate::AppContainer,
        launch: Launch::Container {
            sid: profile.sid,
            exe: &copy,
        },
        workspace: &roots.workspace,
    };
    probe.line(format_args!(
        "build={:?} profile={:?}",
        build(),
        profile.sid_text
    ));
    let identity = probe.run(&["mechanism", "identity"]);
    assert!(
        identity.reported() == Reported::Ok && identity.stdout.contains("app_container=1"),
        "the candidate must run as an AppContainer: {}",
        identity.evidence()
    );
    probe.line(format_args!("identity={:?}", identity.stdout.trim()));
    matrix(
        &probe,
        &roots,
        &Predictions {
            // Both the user and the package or capability SIDs must grant
            // every access (MS10); nothing outside names this package.
            outside_delete: "denied",
            outside_delete_via_parent: "denied",
            outside_rename: "denied",
            foreign_delete: "denied",
            outside_read: "denied",
            // No network capability, and loopback needs an exemption (MS15).
            loopback: "denied",
        },
    );
    lifecycle(&probe);
}

// ---------------------------------------------------------------------------
// Process creation in a Job this test owns.

/// The leading probe argument, followed by a directory, with which a
/// launched candidate sends its own standard output and error to files
/// before it attempts its operation: see [`redirect`].
pub const OUTPUT_ARG: &str = "mechanism-output";

/// The exit status of a candidate that could not redirect its output. It
/// exits with it before attempting any operation.
pub const REDIRECT_FAILED: i32 = 71;

const STDOUT_FILE: &str = "stdout.txt";
const STDERR_FILE: &str = "stderr.txt";

/// The most of each output file the launcher reads back.
const OUTPUT_LIMIT: u64 = 64 * 1024;

/// Creates `dir\stdout.txt` and `dir\stderr.txt`, neither of which may
/// exist, and makes them this process's standard output and error. Rust's
/// standard streams look the handles up on every write, and a child spawned
/// with inherited stdio receives them, so everything after this call lands
/// in the files. The handles stay open for the life of the process.
pub fn redirect(dir: &str) -> std::io::Result<()> {
    use std::os::windows::io::IntoRawHandle;
    use windows_sys::Win32::System::Console::{STD_ERROR_HANDLE, STD_OUTPUT_HANDLE, SetStdHandle};
    let dir = Path::new(dir);
    let stdout = std::fs::File::create_new(dir.join(STDOUT_FILE))?;
    let stderr = std::fs::File::create_new(dir.join(STDERR_FILE))?;
    for (id, file) in [(STD_OUTPUT_HANDLE, stdout), (STD_ERROR_HANDLE, stderr)] {
        // SAFETY: a live handle, deliberately left open.
        check(
            unsafe { SetStdHandle(id, file.into_raw_handle()) },
            "SetStdHandle",
        )?;
    }
    Ok(())
}

/// A fresh directory under the candidate's workspace `cwd` for one launch's
/// output files. The workspace is the one root every candidate can write;
/// no case looks at these names, and the workspace's scratch guard removes
/// them with it.
fn output_dir(cwd: &Path) -> PathBuf {
    use std::sync::atomic::{AtomicUsize, Ordering};
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    let dir = cwd.join(format!(
        ".mechanism-output-{}",
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::create_dir(&dir).unwrap_or_else(|error| panic!("create {}: {error}", dir.display()));
    dir
}

/// At most [`OUTPUT_LIMIT`] bytes of `dir\name`, marked when cut short;
/// `None` if the candidate never created it.
fn read_output(dir: &Path, name: &str) -> Option<String> {
    let path = dir.join(name);
    let file = match std::fs::File::open(&path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return None,
        Err(error) => panic!("open {}: {error}", path.display()),
    };
    let mut bytes = Vec::new();
    file.take(OUTPUT_LIMIT + 1)
        .read_to_end(&mut bytes)
        .unwrap_or_else(|error| panic!("read {}: {error}", path.display()));
    let cut = bytes.len() as u64 > OUTPUT_LIMIT;
    bytes.truncate(OUTPUT_LIMIT as usize);
    let mut text = String::from_utf8_lossy(&bytes).into_owned();
    if cut {
        text.push_str("\n[output truncated]");
    }
    Some(text)
}

/// Starts `exe PROBE_ARG OUTPUT_ARG <dir> ops...` suspended and born in a
/// fresh [`TestJob`], as `token` (`CreateProcessAsUserW`) or as the
/// AppContainer `container` (`CreateProcessW`), resumes it, and waits within
/// [`BOUND`].
///
/// The candidate inherits no handle (`bInheritHandles` is false; its Job
/// comes from the attribute list). It writes its output to files it creates
/// in `<dir>`, a fresh directory under its workspace `cwd`, so this side has
/// no pipe, no reader thread, and no inheritable handle that a process
/// another test starts concurrently could pick up.
///
/// The Job is acquired first, is kill-on-close, and its drop terminates
/// and reaps every member, so a failure at any later step, process
/// creation, resume, or the wait included, leaves no process behind. On
/// the normal path the Job's live members are recorded when the direct
/// process exits (a failed query panics rather than reading as empty),
/// [`TestJob::finish`] must report the Job reaped, and only then are the
/// files read. A launch that overruns [`BOUND`] or leaves no output files
/// panics with whatever evidence there is.
pub(super) fn launch(
    token: Option<&OwnedHandle>,
    container: Option<*mut c_void>,
    exe: &Path,
    ops: &[&str],
    cwd: &Path,
) -> Outcome {
    use windows_sys::Win32::Security::SECURITY_CAPABILITIES;
    use windows_sys::Win32::System::Threading::{
        CREATE_SUSPENDED, CreateProcessAsUserW, CreateProcessW, EXTENDED_STARTUPINFO_PRESENT,
        GetExitCodeProcess, PROC_THREAD_ATTRIBUTE_JOB_LIST,
        PROC_THREAD_ATTRIBUTE_SECURITY_CAPABILITIES, PROCESS_INFORMATION, ResumeThread,
        STARTUPINFOEXW,
    };
    // Declared first, so a panic drops it last and its drop reaps the tree.
    let job = TestJob::new();
    let output = output_dir(cwd);
    let mut argv = vec![
        text(exe),
        PROBE_ARG.to_owned(),
        OUTPUT_ARG.to_owned(),
        text(&output),
    ];
    argv.extend(ops.iter().map(|op| (*op).to_owned()));
    let mut command_line = wide(OsStr::new(&command_line(&argv)));
    let application = wide(exe.as_os_str());
    let directory = wide(cwd.as_os_str());
    let jobs = [job.raw()];
    let capabilities = container.map(|sid| SECURITY_CAPABILITIES {
        AppContainerSid: sid,
        Capabilities: null_mut(),
        CapabilityCount: 0,
        Reserved: 0,
    });
    let mut attributes = Attributes::new(2);
    attributes.set(
        PROC_THREAD_ATTRIBUTE_JOB_LIST,
        jobs.as_ptr().cast(),
        size_of_val(&jobs),
    );
    if let Some(capabilities) = &capabilities {
        attributes.set(
            PROC_THREAD_ATTRIBUTE_SECURITY_CAPABILITIES,
            std::ptr::from_ref(capabilities).cast(),
            size_of::<SECURITY_CAPABILITIES>(),
        );
    }
    // SAFETY: a zeroed STARTUPINFOEXW is valid before its fields are set.
    let mut startup: STARTUPINFOEXW = unsafe { std::mem::zeroed() };
    startup.StartupInfo.cb = size_of::<STARTUPINFOEXW>() as u32;
    startup.lpAttributeList = attributes.pointer();
    // SAFETY: a zeroed PROCESS_INFORMATION is valid output space.
    let mut info: PROCESS_INFORMATION = unsafe { std::mem::zeroed() };
    let flags = CREATE_SUSPENDED | EXTENDED_STARTUPINFO_PRESENT;
    // SAFETY: every string is NUL-terminated, `command_line` is mutable,
    // and the attribute values outlive the call. No handle is inherited; a
    // null environment inherits this process's.
    let created = unsafe {
        match token {
            Some(token) => CreateProcessAsUserW(
                token.as_raw_handle(),
                application.as_ptr(),
                command_line.as_mut_ptr(),
                null(),
                null(),
                0,
                flags,
                null(),
                directory.as_ptr(),
                &startup.StartupInfo,
                &mut info,
            ),
            None => CreateProcessW(
                application.as_ptr(),
                command_line.as_mut_ptr(),
                null(),
                null(),
                0,
                flags,
                null(),
                directory.as_ptr(),
                &startup.StartupInfo,
                &mut info,
            ),
        }
    };
    // Read the error before anything else can overwrite it.
    let created = check(created, "CreateProcess for the candidate");
    drop(attributes);
    created.unwrap_or_else(|error| panic!("the candidate process did not start: {error}"));
    // SAFETY: fresh handles owned here, wrapped before anything can panic.
    let process = unsafe { OwnedHandle::from_raw_handle(info.hProcess) };
    // SAFETY: as above.
    let thread = unsafe { OwnedHandle::from_raw_handle(info.hThread) };
    // SAFETY: a live, suspended thread handle.
    let resumed = unsafe { ResumeThread(thread.as_raw_handle()) };
    assert_ne!(
        resumed,
        u32::MAX,
        "ResumeThread: {}",
        std::io::Error::last_os_error()
    );
    let exited = signaled_within(process.as_raw_handle(), BOUND);
    // The full 32-bit status, so an NTSTATUS such as 0xC0000142 stays
    // readable in the evidence.
    let code = exited.then(|| {
        let mut code = 0u32;
        // SAFETY: a live process handle and a valid out pointer.
        check(
            unsafe { GetExitCodeProcess(process.as_raw_handle(), &mut code) },
            "GetExitCodeProcess",
        )
        .expect("exit code");
        code as i32
    });
    // A failed query panics here, and the Job's drop still reaps the tree.
    let left_in_job = job
        .members()
        .unwrap_or_else(|error| panic!("the candidate's Job members: {error}"));
    drop(thread);
    drop(process);
    // Terminates every member, the direct process too if it overran, and
    // waits until the Job counts none active.
    job.finish()
        .unwrap_or_else(|error| panic!("the candidate's Job was not reaped: {error}"));
    let stdout = read_output(&output, STDOUT_FILE);
    let stderr = read_output(&output, STDERR_FILE);
    assert!(
        exited,
        "the candidate process did not exit within the bound: stdout={stdout:?} stderr={stderr:?}"
    );
    let (Some(stdout), Some(stderr)) = (stdout, stderr) else {
        let why = if code == Some(REDIRECT_FAILED) {
            "it could not create them"
        } else {
            "it ended before creating them; a start failure such as 0xC0000142 shows in the status"
        };
        panic!(
            "the candidate left no output files in {} ({why}): exit={code:?} ({:#010x})",
            output.display(),
            code.unwrap_or_default()
        );
    };
    Outcome {
        code,
        stdout,
        stderr,
        left_in_job,
    }
}

/// The command line `CommandLineToArgvW` splits back into `argv`.
fn command_line(argv: &[String]) -> String {
    let mut line = String::new();
    for arg in argv {
        if !line.is_empty() {
            line.push(' ');
        }
        line.push('"');
        let mut backslashes = 0;
        for character in arg.chars() {
            match character {
                '\\' => backslashes += 1,
                '"' => {
                    line.extend(std::iter::repeat_n('\\', backslashes * 2 + 1));
                    line.push('"');
                    backslashes = 0;
                }
                other => {
                    line.extend(std::iter::repeat_n('\\', backslashes));
                    line.push(other);
                    backslashes = 0;
                }
            }
        }
        line.extend(std::iter::repeat_n('\\', backslashes * 2));
        line.push('"');
    }
    line
}

/// An initialized `PROC_THREAD_ATTRIBUTE_LIST`, deleted on drop.
struct Attributes(Vec<u64>);

impl Attributes {
    fn new(count: u32) -> Self {
        use windows_sys::Win32::System::Threading::InitializeProcThreadAttributeList;
        let mut size = 0usize;
        // SAFETY: a size query; it fails and reports the size.
        unsafe { InitializeProcThreadAttributeList(null_mut(), count, 0, &mut size) };
        assert_ne!(size, 0, "InitializeProcThreadAttributeList size");
        let mut buffer = vec![0u64; size.div_ceil(8)];
        // SAFETY: `buffer` holds `size` bytes.
        let ok = unsafe {
            InitializeProcThreadAttributeList(buffer.as_mut_ptr().cast(), count, 0, &mut size)
        };
        check(ok, "InitializeProcThreadAttributeList").expect("attribute list");
        Self(buffer)
    }

    fn pointer(&mut self) -> *mut c_void {
        self.0.as_mut_ptr().cast()
    }

    /// Sets `attribute` to the `size` bytes at `value`, which must outlive
    /// the process creation.
    fn set(&mut self, attribute: u32, value: *const c_void, size: usize) {
        use windows_sys::Win32::System::Threading::UpdateProcThreadAttribute;
        // SAFETY: the list is initialized; the caller keeps `value` alive.
        let ok = unsafe {
            UpdateProcThreadAttribute(
                self.pointer(),
                0,
                attribute as usize,
                value,
                size,
                null_mut(),
                null(),
            )
        };
        check(ok, "UpdateProcThreadAttribute").expect("process attribute");
    }
}

impl Drop for Attributes {
    fn drop(&mut self) {
        use windows_sys::Win32::System::Threading::DeleteProcThreadAttributeList;
        // SAFETY: the list was initialized and is deleted once.
        unsafe { DeleteProcThreadAttributeList(self.pointer()) }
    }
}

// ---------------------------------------------------------------------------
// The AppContainer profile.

/// An AppContainer profile with a name no other run uses. Dropped, it is
/// deleted, its registry and folder state with it, and its SID freed.
pub(super) struct Profile {
    name: Vec<u16>,
    pub(super) sid: *mut c_void,
    pub(super) sid_text: String,
}

impl Profile {
    pub(super) fn create() -> Self {
        use std::sync::atomic::{AtomicUsize, Ordering};
        use windows_sys::Win32::Security::Isolation::CreateAppContainerProfile;
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |elapsed| elapsed.subsec_nanos());
        // At most 64 characters, as the API requires.
        let moniker = format!(
            "bake.probe.{}.{}.{nanos}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        );
        let name = wide(OsStr::new(&moniker));
        let display = wide(OsStr::new("Bake sandbox mechanism probe"));
        let mut sid = null_mut();
        // SAFETY: NUL-terminated strings, no capabilities, a valid out
        // pointer. A name that already exists fails here and is never
        // deleted by this run.
        let status = unsafe {
            CreateAppContainerProfile(
                name.as_ptr(),
                display.as_ptr(),
                display.as_ptr(),
                null(),
                0,
                &mut sid,
            )
        };
        assert!(
            status >= 0,
            "CreateAppContainerProfile({moniker}): HRESULT {status:#010x}"
        );
        // The guard exists before anything else can panic, so a profile this
        // run created is always deleted.
        let mut profile = Self {
            name,
            sid,
            sid_text: String::new(),
        };
        assert!(
            !sid.is_null(),
            "CreateAppContainerProfile({moniker}) returned no SID"
        );
        profile.sid_text = win32::sid_text(sid);
        profile
    }
}

impl Drop for Profile {
    fn drop(&mut self) {
        use windows_sys::Win32::Security::FreeSid;
        use windows_sys::Win32::Security::Isolation::DeleteAppContainerProfile;
        // SAFETY: the name this run created.
        let status = unsafe { DeleteAppContainerProfile(self.name.as_ptr()) };
        if !self.sid.is_null() {
            // SAFETY: allocated by CreateAppContainerProfile, freed once.
            unsafe { FreeSid(self.sid) };
        }
        if !std::thread::panicking() {
            assert!(
                status >= 0,
                "DeleteAppContainerProfile: HRESULT {status:#010x}"
            );
        }
    }
}

// ---------------------------------------------------------------------------
// The low-integrity candidate token.

pub(super) mod token {
    use std::ffi::{OsStr, c_void};
    use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
    use std::ptr::{null, null_mut};

    use windows_sys::Win32::Security::Authorization::{
        EXPLICIT_ACCESS_W, GRANT_ACCESS, NO_MULTIPLE_TRUSTEE, SetEntriesInAclW, TRUSTEE_IS_SID,
        TRUSTEE_IS_UNKNOWN, TRUSTEE_W,
    };
    use windows_sys::Win32::Security::{
        ACL, CreateRestrictedToken, DISABLE_MAX_PRIVILEGE, GetLengthSid, LUA_TOKEN,
        SID_AND_ATTRIBUTES, SUB_CONTAINERS_AND_OBJECTS_INHERIT, SetTokenInformation,
        TOKEN_ADJUST_DEFAULT, TOKEN_ASSIGN_PRIMARY, TOKEN_DEFAULT_DACL, TOKEN_DUPLICATE,
        TOKEN_GROUPS, TOKEN_MANDATORY_LABEL, TOKEN_QUERY, TokenDefaultDacl, TokenGroups,
        TokenIntegrityLevel, WRITE_RESTRICTED,
    };
    use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

    use super::{Local, check, token_information, wide};

    /// `SE_GROUP_LOGON_ID`.
    const LOGON_ID: u32 = 0xC000_0000;
    /// `SE_GROUP_INTEGRITY`.
    const INTEGRITY: u32 = 0x20;
    const FILE_ALL_ACCESS: u32 = 0x001F_01FF;

    /// A SID from its string form, freed on drop.
    pub(in super::super) struct Sid(Local);

    impl Sid {
        pub(in super::super) fn parse(text: &str) -> std::io::Result<Self> {
            use windows_sys::Win32::Security::Authorization::ConvertStringSidToSidW;
            let units = wide(OsStr::new(text));
            let mut sid = null_mut();
            // SAFETY: NUL-terminated input; the SID is LocalAlloc'd.
            check(
                unsafe { ConvertStringSidToSidW(units.as_ptr(), &mut sid) },
                "ConvertStringSidToSidW",
            )?;
            Ok(Self(Local(sid)))
        }

        pub(in super::super) fn as_ptr(&self) -> *mut c_void {
            self.0.0
        }
    }

    /// The runner's token, built as `restricted_token` builds it (the same
    /// flags, restricting SIDs, and default-DACL grant, for a read-only or
    /// one-capability run), then set to the low integrity level (MS14).
    pub fn low_integrity(capability: &str) -> std::io::Result<OwnedHandle> {
        let mut raw = null_mut();
        // SAFETY: the pseudo-handle needs no closing.
        check(
            unsafe {
                OpenProcessToken(
                    GetCurrentProcess(),
                    TOKEN_DUPLICATE | TOKEN_QUERY | TOKEN_ASSIGN_PRIMARY | TOKEN_ADJUST_DEFAULT,
                    &mut raw,
                )
            },
            "OpenProcessToken",
        )?;
        // SAFETY: a fresh handle owned here.
        let current = unsafe { OwnedHandle::from_raw_handle(raw) };
        let groups = token_information(&current, TokenGroups)?;
        let base = groups.as_ptr().cast::<TOKEN_GROUPS>();
        // SAFETY: the buffer holds a TOKEN_GROUPS whose array lies in it.
        let logon = unsafe {
            std::slice::from_raw_parts((*base).Groups.as_ptr(), (*base).GroupCount as usize)
        }
        .iter()
        .find(|group| group.Attributes & LOGON_ID == LOGON_ID)
        .map(|group| group.Sid)
        .ok_or_else(|| std::io::Error::other("no logon SID in the token groups"))?;
        let everyone = Sid::parse("S-1-1-0")?;
        let capability = Sid::parse(capability)?;
        let restricting =
            [logon, everyone.as_ptr(), capability.as_ptr()].map(|sid| SID_AND_ATTRIBUTES {
                Sid: sid,
                Attributes: 0,
            });
        let mut raw = null_mut();
        // SAFETY: `restricting` holds valid SIDs (`groups` keeps the logon
        // SID alive) that outlive the call.
        check(
            unsafe {
                CreateRestrictedToken(
                    current.as_raw_handle(),
                    DISABLE_MAX_PRIVILEGE | LUA_TOKEN | WRITE_RESTRICTED,
                    0,
                    null(),
                    0,
                    null(),
                    restricting.len() as u32,
                    restricting.as_ptr(),
                    &mut raw,
                )
            },
            "CreateRestrictedToken",
        )?;
        // SAFETY: a fresh handle owned here.
        let token = unsafe { OwnedHandle::from_raw_handle(raw) };
        grant_default_dacl(&token, &everyone)?;
        let low = Sid::parse("S-1-16-4096")?;
        let label = TOKEN_MANDATORY_LABEL {
            Label: SID_AND_ATTRIBUTES {
                Sid: low.as_ptr(),
                Attributes: INTEGRITY,
            },
        };
        // SAFETY: a valid SID; the call copies the label.
        let length =
            size_of::<TOKEN_MANDATORY_LABEL>() as u32 + unsafe { GetLengthSid(low.as_ptr()) };
        // SAFETY: `label` points at a valid SID for the length passed.
        check(
            unsafe {
                SetTokenInformation(
                    token.as_raw_handle(),
                    TokenIntegrityLevel,
                    std::ptr::addr_of!(label).cast(),
                    length,
                )
            },
            "SetTokenInformation(TokenIntegrityLevel)",
        )?;
        Ok(token)
    }

    /// `setTokenDefaultDaclGrant` with Everyone, as a read-only run does.
    fn grant_default_dacl(token: &OwnedHandle, everyone: &Sid) -> std::io::Result<()> {
        let buffer = token_information(token, TokenDefaultDacl)?;
        // SAFETY: the buffer holds a TOKEN_DEFAULT_DACL whose ACL lies in it.
        let current = unsafe { (*buffer.as_ptr().cast::<TOKEN_DEFAULT_DACL>()).DefaultDacl };
        if current.is_null() {
            return Err(std::io::Error::other("the token has no default DACL"));
        }
        let entry = EXPLICIT_ACCESS_W {
            grfAccessPermissions: FILE_ALL_ACCESS,
            grfAccessMode: GRANT_ACCESS,
            grfInheritance: SUB_CONTAINERS_AND_OBJECTS_INHERIT,
            Trustee: TRUSTEE_W {
                pMultipleTrustee: null_mut(),
                MultipleTrusteeOperation: NO_MULTIPLE_TRUSTEE,
                TrusteeForm: TRUSTEE_IS_SID,
                TrusteeType: TRUSTEE_IS_UNKNOWN,
                ptstrName: everyone.as_ptr().cast(),
            },
        };
        let mut merged: *mut ACL = null_mut();
        // SAFETY: one valid entry and a valid ACL.
        let status = unsafe { SetEntriesInAclW(1, &entry, current, &mut merged) };
        if status != 0 {
            return Err(std::io::Error::from_raw_os_error(status as i32));
        }
        let merged = Local(merged.cast());
        let info = TOKEN_DEFAULT_DACL {
            DefaultDacl: merged.0.cast(),
        };
        // SAFETY: `info` points at a valid ACL; the call copies it.
        check(
            unsafe {
                SetTokenInformation(
                    token.as_raw_handle(),
                    TokenDefaultDacl,
                    std::ptr::addr_of!(info).cast(),
                    size_of::<TOKEN_DEFAULT_DACL>() as u32,
                )
            },
            "SetTokenInformation(TokenDefaultDacl)",
        )
    }
}

// ---------------------------------------------------------------------------
// Fixture ACLs through Win32, with binary SIDs.

pub(super) mod acl {
    use std::ffi::c_void;
    use std::path::Path;
    use std::ptr::{null, null_mut};

    use windows_sys::Win32::Security::Authorization::{
        EXPLICIT_ACCESS_W, GRANT_ACCESS, GetNamedSecurityInfoW, NO_MULTIPLE_TRUSTEE,
        SE_FILE_OBJECT, SetEntriesInAclW, SetNamedSecurityInfoW, TRUSTEE_IS_SID,
        TRUSTEE_IS_UNKNOWN, TRUSTEE_W,
    };
    use windows_sys::Win32::Security::{
        ACCESS_ALLOWED_ACE, ACE_HEADER, ACL, DACL_SECURITY_INFORMATION, GetAce,
        LABEL_SECURITY_INFORMATION, PSECURITY_DESCRIPTOR, SUB_CONTAINERS_AND_OBJECTS_INHERIT,
    };

    use super::{Local, check, token::Sid, wide, win32};

    /// `ACCESS_ALLOWED_ACE_TYPE`.
    pub const ACCESS_ALLOWED: u8 = 0x0;
    /// `ACCESS_DENIED_ACE_TYPE`, laid out as an allow ACE.
    pub const ACCESS_DENIED: u8 = 0x1;
    /// `SYSTEM_MANDATORY_LABEL_ACE_TYPE`, laid out as an allow ACE.
    pub const MANDATORY_LABEL: u8 = 0x11;
    /// `OBJECT_INHERIT_ACE | CONTAINER_INHERIT_ACE`, and nothing else: not
    /// inherited, not inherit-only, propagated.
    pub const OI_CI: u8 = 0x3;
    /// `SYSTEM_MANDATORY_LABEL_NO_WRITE_UP`.
    pub const NO_WRITE_UP: u32 = 0x1;
    const _: () = assert!(SUB_CONTAINERS_AND_OBJECTS_INHERIT == OI_CI as u32);

    /// One ACE as read back: type, flags, mask, and SID as text (empty for
    /// a type whose SID is not at `SidStart`).
    #[derive(Clone, Debug, PartialEq, Eq)]
    pub struct Ace {
        pub kind: u8,
        pub flags: u8,
        pub mask: u32,
        pub sid: String,
    }

    /// A root's DACL and mandatory label ACEs, in stored order.
    pub struct Descriptor {
        pub dacl: Vec<Ace>,
        pub label: Vec<Ace>,
    }

    impl Descriptor {
        pub fn read(path: &Path) -> std::io::Result<Self> {
            let (_descriptor, dacl, sacl) =
                named_security(path, DACL_SECURITY_INFORMATION | LABEL_SECURITY_INFORMATION)?;
            Ok(Self {
                dacl: aces(dacl)?,
                label: aces(sacl)?,
            })
        }
    }

    /// `path`'s descriptor with the requested parts, and its DACL and SACL
    /// pointers, which point into the descriptor (null when absent).
    fn named_security(path: &Path, parts: u32) -> std::io::Result<(Local, *mut ACL, *mut ACL)> {
        let units = wide(path.as_os_str());
        let (mut dacl, mut sacl): (*mut ACL, *mut ACL) = (null_mut(), null_mut());
        let mut descriptor: PSECURITY_DESCRIPTOR = null_mut();
        // SAFETY: NUL-terminated name and valid out pointers; the ACLs point
        // into the descriptor, which `Local` frees.
        let status = unsafe {
            GetNamedSecurityInfoW(
                units.as_ptr(),
                SE_FILE_OBJECT,
                parts,
                null_mut(),
                null_mut(),
                &mut dacl,
                &mut sacl,
                &mut descriptor,
            )
        };
        let descriptor = Local(descriptor);
        if status != 0 {
            return Err(win32_error("GetNamedSecurityInfoW", status));
        }
        Ok((descriptor, dacl, sacl))
    }

    fn win32_error(api: &str, status: u32) -> std::io::Error {
        let error = std::io::Error::from_raw_os_error(status as i32);
        std::io::Error::new(error.kind(), format!("{api}: {error}"))
    }

    /// Every ACE of `list`, which must point into a live descriptor.
    fn aces(list: *mut ACL) -> std::io::Result<Vec<Ace>> {
        if list.is_null() {
            return Ok(Vec::new());
        }
        // SAFETY: a valid ACL in a live descriptor.
        let count = unsafe { (*list).AceCount };
        (0..u32::from(count))
            .map(|index| {
                let mut ace: *mut c_void = null_mut();
                // SAFETY: `index` is below the ACL's ACE count.
                check(unsafe { GetAce(list, index, &mut ace) }, "GetAce")?;
                // SAFETY: every ACE starts with an ACE_HEADER.
                let header = unsafe { *ace.cast::<ACE_HEADER>() };
                let simple = matches!(
                    header.AceType,
                    ACCESS_ALLOWED | ACCESS_DENIED | MANDATORY_LABEL
                );
                if usize::from(header.AceSize) < size_of::<ACCESS_ALLOWED_ACE>() {
                    return Err(std::io::Error::other(format!(
                        "ACE {index} is {} bytes",
                        header.AceSize
                    )));
                }
                let body = ace.cast::<ACCESS_ALLOWED_ACE>();
                // SAFETY: every ACE of at least this size has its mask after
                // the header; the simple types hold their SID at `SidStart`.
                let (mask, sid) = unsafe {
                    (
                        (*body).Mask,
                        if simple {
                            win32::sid_text(std::ptr::addr_of_mut!((*body).SidStart).cast())
                        } else {
                            String::new()
                        },
                    )
                };
                Ok(Ace {
                    kind: header.AceType,
                    flags: header.AceFlags,
                    mask,
                    sid,
                })
            })
            .collect()
    }

    /// Merges an `OI|CI` allow ACE for `sid` with `mask` into `path`'s DACL
    /// and writes back only the DACL, so the owner and label stay as they
    /// are. The runner's `grant_write` makes the same two calls.
    pub fn grant(path: &Path, sid: &Sid, mask: u32) -> std::io::Result<()> {
        let (_descriptor, dacl, _) = named_security(path, DACL_SECURITY_INFORMATION)?;
        let entry = EXPLICIT_ACCESS_W {
            grfAccessPermissions: mask,
            grfAccessMode: GRANT_ACCESS,
            grfInheritance: SUB_CONTAINERS_AND_OBJECTS_INHERIT,
            Trustee: TRUSTEE_W {
                pMultipleTrustee: null_mut(),
                MultipleTrusteeOperation: NO_MULTIPLE_TRUSTEE,
                TrusteeForm: TRUSTEE_IS_SID,
                TrusteeType: TRUSTEE_IS_UNKNOWN,
                ptstrName: sid.as_ptr().cast(),
            },
        };
        let mut merged: *mut ACL = null_mut();
        // SAFETY: one valid entry whose SID outlives the call; `dacl` is null
        // or a valid ACL in the live descriptor.
        let status = unsafe { SetEntriesInAclW(1, &entry, dacl, &mut merged) };
        let merged = Local(merged.cast());
        if status != 0 || merged.0.is_null() {
            return Err(win32_error("SetEntriesInAclW", status));
        }
        let units = wide(path.as_os_str());
        // SAFETY: NUL-terminated name and a valid ACL; only the DACL is set.
        let status = unsafe {
            SetNamedSecurityInfoW(
                units.as_ptr(),
                SE_FILE_OBJECT,
                DACL_SECURITY_INFORMATION,
                null_mut(),
                null_mut(),
                merged.0.cast(),
                null(),
            )
        };
        if status != 0 {
            return Err(win32_error("SetNamedSecurityInfoW", status));
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Small Win32 helpers.

/// A `LocalAlloc`'d pointer, freed on drop.
struct Local(*mut c_void);

impl Drop for Local {
    fn drop(&mut self) {
        use windows_sys::Win32::Foundation::LocalFree;
        if !self.0.is_null() {
            // SAFETY: allocated by the API that returned it, freed once.
            unsafe { LocalFree(self.0) };
        }
    }
}

fn wide(text: &OsStr) -> Vec<u16> {
    text.encode_wide().chain([0]).collect()
}

fn check(ok: i32, what: &str) -> std::io::Result<()> {
    if ok == 0 {
        let error = std::io::Error::last_os_error();
        Err(std::io::Error::new(
            error.kind(),
            format!("{what}: {error}"),
        ))
    } else {
        Ok(())
    }
}

fn token_information(token: &OwnedHandle, class: i32) -> std::io::Result<Vec<u64>> {
    use windows_sys::Win32::Security::GetTokenInformation;
    let mut needed = 0u32;
    // SAFETY: a size query.
    unsafe { GetTokenInformation(token.as_raw_handle(), class, null_mut(), 0, &mut needed) };
    let mut buffer = vec![0u64; (needed as usize).div_ceil(8).max(1)];
    // SAFETY: `buffer` holds at least `needed` bytes.
    check(
        unsafe {
            GetTokenInformation(
                token.as_raw_handle(),
                class,
                buffer.as_mut_ptr().cast(),
                (buffer.len() * 8) as u32,
                &mut needed,
            )
        },
        "GetTokenInformation",
    )?;
    Ok(buffer)
}
