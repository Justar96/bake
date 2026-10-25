//! Pi's `src/utils/shell.ts`: shell resolution and the shell environment,
//! with v1.4.0's legacy WSL detection under `%SystemRoot%` and existence
//! checks that accept Microsoft Store aliases. Pi's `getPowerShellConfig`
//! is not ported, as its `powershell` tool is not.
//!
//! Pi finds executables on `PATH` by running `which` (Unix) or `where`
//! (Windows); this port searches `PATH` itself, so resolution starts no
//! process and cannot hang. `where` also searches the current directory
//! first; this port does not.

use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};

/// How the command reaches the shell, Pi's `commandTransport`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum CommandTransport {
    /// The command is the last argument.
    #[default]
    Argv,
    /// The command is written to standard input, which is then closed.
    Stdin,
}

/// A resolved shell, Pi's `ShellConfig`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShellConfig {
    /// The shell executable.
    pub shell: PathBuf,
    /// Arguments before the command.
    pub args: Vec<String>,
    /// How the command reaches the shell.
    pub command_transport: CommandTransport,
}

/// Pi's `isLegacyWslBashPath` with this process's `%SystemRoot%`; see
/// [`is_legacy_wsl_bash_path_under`].
pub fn is_legacy_wsl_bash_path(path: &Path) -> bool {
    is_legacy_wsl_bash_path_under(path, std::env::var("SystemRoot").ok().as_deref())
}

/// Pi's `isLegacyWslBashPath`: `System32\bash.exe` or `Sysnative\bash.exe`
/// under the Windows directory `system_root`, in any case and with either
/// slash. A `system_root` that is not drive-absolute matches nothing;
/// without one, `X:\Windows` on any drive is assumed.
pub fn is_legacy_wsl_bash_path_under(path: &Path, system_root: Option<&str>) -> bool {
    let normalized = normalize_windows_path(&path.to_string_lossy()).to_lowercase();
    if let Some(root) = system_root.filter(|root| !root.is_empty()) {
        if !is_drive_absolute(root) {
            return false;
        }
        return ["System32", "Sysnative"].iter().any(|directory| {
            normalized
                == normalize_windows_path(&format!("{root}\\{directory}\\bash.exe")).to_lowercase()
        });
    }
    if !is_drive_absolute(&normalized) {
        return false;
    }
    let rest = &normalized[3..];
    rest == "windows\\system32\\bash.exe" || rest == "windows\\sysnative\\bash.exe"
}

/// Whether `path` opens with a drive letter, a colon, and a separator.
fn is_drive_absolute(path: &str) -> bool {
    matches!(path.as_bytes(), [drive, b':', b'\\' | b'/', ..] if drive.is_ascii_alphabetic())
}

/// Node's `path.win32.normalize` for a drive-absolute path: forward
/// slashes become backslashes, repeated separators collapse, and `.` and
/// `..` segments resolve. Any other path is returned with its slashes
/// turned, which no drive-absolute path then equals.
fn normalize_windows_path(path: &str) -> String {
    let path = path.replace('/', "\\");
    if !is_drive_absolute(&path) {
        return path;
    }
    let mut parts: Vec<&str> = Vec::new();
    for part in path[3..].split('\\') {
        match part {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            part => parts.push(part),
        }
    }
    format!("{}{}", &path[..3], parts.join("\\"))
}

/// Pi's `shellPathExists`, `access(F_OK)`. On Windows, `Path::exists`
/// follows reparse points and so rejects runnable Microsoft Store aliases;
/// the alias itself is accepted there.
pub fn shell_path_exists(path: &Path) -> bool {
    #[cfg(windows)]
    {
        path.exists() || std::fs::symlink_metadata(path).is_ok()
    }
    #[cfg(not(windows))]
    {
        path.exists()
    }
}

/// Pi's `getBashShellConfig`: the legacy WSL launcher reads the command
/// from standard input (`-s`); every other bash takes `-c <command>`.
pub fn bash_shell_config(shell: PathBuf) -> ShellConfig {
    if is_legacy_wsl_bash_path(&shell) {
        ShellConfig {
            shell,
            args: vec!["-s".to_owned()],
            command_transport: CommandTransport::Stdin,
        }
    } else {
        ShellConfig {
            shell,
            args: vec!["-c".to_owned()],
            command_transport: CommandTransport::Argv,
        }
    }
}

/// Pi's `getShellConfig`. Resolution order: the given shell path; on
/// Windows, Git Bash under `%ProgramFiles%` and `%ProgramFiles(x86)%`, then
/// `bash.exe` on `PATH`; on Unix, `/bin/bash`, then `bash` on `PATH`, then
/// `sh`. `Err` holds Pi's message.
pub fn get_shell_config(custom_shell_path: Option<&Path>) -> Result<ShellConfig, String> {
    if let Some(path) = custom_shell_path.filter(|path| !path.as_os_str().is_empty()) {
        if shell_path_exists(path) {
            return Ok(bash_shell_config(path.to_path_buf()));
        }
        return Err(format!("Custom shell path not found: {}", path.display()));
    }
    #[cfg(windows)]
    {
        windows_shell_config(
            std::env::var("ProgramFiles").ok().as_deref(),
            std::env::var("ProgramFiles(x86)").ok().as_deref(),
            shell_path_exists,
            || find_executable_on_path("bash.exe"),
        )
    }
    #[cfg(not(windows))]
    {
        let bin_bash = Path::new("/bin/bash");
        if shell_path_exists(bin_bash) {
            return Ok(bash_shell_config(bin_bash.to_path_buf()));
        }
        if let Some(bash) = find_executable_on_path("bash") {
            return Ok(bash_shell_config(bash));
        }
        Ok(ShellConfig {
            shell: PathBuf::from("sh"),
            args: vec!["-c".to_owned()],
            command_transport: CommandTransport::Argv,
        })
    }
}

/// The Windows branch of Pi's `getShellConfig`, with its inputs passed in
/// so every platform can test it. Pi joins the Git Bash paths as strings.
pub fn windows_shell_config(
    program_files: Option<&str>,
    program_files_x86: Option<&str>,
    exists: impl Fn(&Path) -> bool,
    bash_on_path: impl FnOnce() -> Option<PathBuf>,
) -> Result<ShellConfig, String> {
    let paths: Vec<String> = [program_files, program_files_x86]
        .into_iter()
        .flatten()
        .filter(|root| !root.is_empty())
        .map(|root| format!("{root}\\Git\\bin\\bash.exe"))
        .collect();
    if let Some(path) = paths.iter().find(|path| exists(Path::new(path))) {
        return Ok(bash_shell_config(PathBuf::from(path)));
    }
    if let Some(bash) = bash_on_path() {
        return Ok(bash_shell_config(bash));
    }
    let searched: Vec<String> = paths.iter().map(|path| format!("  {path}")).collect();
    Err(format!(
        "No bash shell found. Options:\n  1. Install Git for Windows: https://git-scm.com/download/win\n  2. Add your bash to PATH (Cygwin, MSYS2, etc.)\n  3. Set shellPath in settings.json\n\nSearched Git Bash in:\n{}",
        searched.join("\n")
    ))
}

/// The first `PATH` entry holding `executable` as a file (executable by
/// someone, on Unix), Pi's `findExecutableOnPath` without its subprocess.
pub fn find_executable_on_path(executable: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .filter(|directory| !directory.as_os_str().is_empty())
        .map(|directory| directory.join(executable))
        .find(|candidate| is_executable_file(candidate))
}

fn is_executable_file(path: &Path) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::metadata(path)
            .is_ok_and(|metadata| metadata.is_file() && metadata.permissions().mode() & 0o111 != 0)
    }
    #[cfg(not(unix))]
    {
        // A Microsoft Store alias is a reparse point `metadata` cannot
        // follow; Pi accepts it from `where` through `shellPathExists`.
        match std::fs::metadata(path) {
            Ok(metadata) => metadata.is_file(),
            Err(_) => std::fs::symlink_metadata(path).is_ok_and(|metadata| !metadata.is_dir()),
        }
    }
}

#[cfg(windows)]
const PATH_DELIMITER: &str = ";";
#[cfg(not(windows))]
const PATH_DELIMITER: &str = ":";

/// Pi's `getShellEnv`: this process's environment, with `bin_dir` (Pi's
/// `getBinDir()`, the agent's managed binaries) put first on `PATH` unless
/// already listed. The `PATH` key is found case-insensitively, as on
/// Windows. Bake removes inherited credential-shaped names through
/// [`bake_process::environment::scrub_environment`] before adding the path.
pub fn get_shell_env(bin_dir: Option<&Path>) -> Vec<(OsString, OsString)> {
    shell_env_from(
        bake_process::environment::scrub_environment(std::env::vars_os()).collect(),
        bin_dir,
    )
}

pub(super) fn shell_env_from(
    mut env: Vec<(OsString, OsString)>,
    bin_dir: Option<&Path>,
) -> Vec<(OsString, OsString)> {
    let Some(bin_dir) = bin_dir else {
        return env;
    };
    let position = env
        .iter()
        .position(|(key, _)| key.to_string_lossy().eq_ignore_ascii_case("path"));
    let current = position.map_or_else(OsString::new, |index| env[index].1.clone());
    if path_list_contains(&current, bin_dir) {
        return env;
    }
    let mut updated = bin_dir.as_os_str().to_owned();
    if !current.is_empty() {
        updated.push(PATH_DELIMITER);
        updated.push(&current);
    }
    match position {
        Some(index) => env[index].1 = updated,
        None => env.push((OsString::from("PATH"), updated)),
    }
    env
}

/// Pi's `currentPath.split(delimiter).filter(Boolean).includes(binDir)`.
/// Unix compares raw bytes, so a non-Unicode entry still matches exactly;
/// Windows compares the lossy text, where `PATH` is UTF-16 anyway.
fn path_list_contains(list: &OsStr, directory: &Path) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        list.as_bytes()
            .split(|byte| *byte == b':')
            .any(|entry| !entry.is_empty() && entry == directory.as_os_str().as_bytes())
    }
    #[cfg(not(unix))]
    {
        let directory = directory.to_string_lossy();
        list.to_string_lossy()
            .split(';')
            .any(|entry| !entry.is_empty() && entry == directory)
    }
}
