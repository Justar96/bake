//! Puts selected text on the clipboard, as the TypeScript
//! `writeClipboardText` does.
//!
//! Over SSH the terminal's own clipboard is the user's, so OSC 52 goes first.
//! Otherwise macOS uses `pbcopy`, Windows `clip`, Wayland `wl-copy`, and X11
//! `xclip` then `xsel`. WSL uses OSC 52 under Windows Terminal, which honours
//! it, and `clip.exe` elsewhere. When no tool succeeds, OSC 52 is the last
//! resort; a terminal that ignores it gives no sign, so it counts as copied.

use std::io::Write;
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

/// Longest a clipboard tool may take; one that hangs is killed and reads as
/// failed.
const FEED: Duration = Duration::from_secs(5);
/// How often a running tool is checked for having exited.
const POLL: Duration = Duration::from_millis(10);

/// The operating system the clipboard belongs to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Platform {
    MacOs,
    Windows,
    Other,
}

impl Platform {
    pub fn current() -> Self {
        if cfg!(target_os = "macos") {
            Self::MacOs
        } else if cfg!(windows) {
            Self::Windows
        } else {
            Self::Other
        }
    }
}

/// A program that takes text on its standard input and sets the clipboard.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Tool {
    pub program: &'static str,
    pub args: &'static [&'static str],
    /// `clip` reads UTF-16 when the input starts with its byte order mark,
    /// and the console code page otherwise.
    pub utf16: bool,
}

const fn tool(program: &'static str, args: &'static [&'static str], utf16: bool) -> Tool {
    Tool {
        program,
        args,
        utf16,
    }
}

/// Where copied text goes, read once from the environment.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Clipboard {
    platform: Platform,
    ssh: bool,
    wsl: bool,
    windows_terminal: bool,
    wayland: bool,
}

impl Clipboard {
    pub fn new(platform: Platform, env: impl Fn(&str) -> Option<String>) -> Self {
        let set = |name: &str| env(name).is_some_and(|value| !value.is_empty());
        Self {
            platform,
            ssh: set("SSH_TTY") || set("SSH_CONNECTION"),
            wsl: set("WSL_DISTRO_NAME"),
            windows_terminal: set("WT_SESSION"),
            wayland: set("WAYLAND_DISPLAY"),
        }
    }

    /// Whether the terminal itself is asked first, over SSH.
    pub fn terminal_first(self) -> bool {
        self.ssh
    }

    /// The tools to try, in order, before the terminal's own clipboard.
    pub fn tools(self) -> Vec<Tool> {
        if self.ssh {
            return Vec::new();
        }
        match self.platform {
            Platform::MacOs => vec![tool("pbcopy", &[], false)],
            Platform::Windows => vec![tool("clip", &[], true)],
            Platform::Other if self.wsl && self.windows_terminal => Vec::new(),
            Platform::Other if self.wsl => vec![tool("clip.exe", &[], true)],
            Platform::Other => {
                let mut tools = Vec::new();
                if self.wayland {
                    tools.push(tool("wl-copy", &[], false));
                }
                tools.push(tool("xclip", &["-selection", "clipboard"], false));
                tools.push(tool("xsel", &["--clipboard", "--input"], false));
                tools
            }
        }
    }
}

/// Runs `tool` with `text` on its standard input. Returns whether it exited
/// successfully within [`FEED`]; a missing tool, a failure, or a hang is
/// `false`, and a hung tool is killed.
pub fn feed(tool: Tool, text: &str) -> bool {
    let Ok(mut child) = Command::new(tool.program)
        .args(tool.args)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
    else {
        return false;
    };
    let input = if tool.utf16 {
        let mut bytes = vec![0xff, 0xfe];
        bytes.extend(text.encode_utf16().flat_map(u16::to_le_bytes));
        bytes
    } else {
        text.as_bytes().to_vec()
    };
    // Dropped after writing, so the tool sees the end of its input.
    let written = child
        .stdin
        .take()
        .is_some_and(|mut stdin| stdin.write_all(&input).is_ok());
    let deadline = Instant::now() + FEED;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return written && status.success(),
            Ok(None) if Instant::now() < deadline => thread::sleep(POLL),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return false;
            }
        }
    }
}

/// The OSC 52 sequence that asks the terminal itself to set its clipboard,
/// which reaches the local clipboard over SSH and inside multiplexers that
/// forward it.
pub fn osc52(text: &str) -> String {
    format!("\x1b]52;c;{}\x07", base64(text.as_bytes()))
}

/// Standard Base64 with padding.
fn base64(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let b = [
            chunk[0],
            chunk.get(1).copied().unwrap_or(0),
            chunk.get(2).copied().unwrap_or(0),
        ];
        let n = (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);
        for i in 0..4 {
            if i <= chunk.len() {
                out.push(char::from(ALPHABET[(n >> (18 - 6 * i)) as usize & 63]));
            } else {
                out.push('=');
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn clipboard(platform: Platform, vars: &[(&str, &str)]) -> Clipboard {
        let vars: Vec<(String, String)> = vars
            .iter()
            .map(|&(k, v)| (k.to_owned(), v.to_owned()))
            .collect();
        Clipboard::new(platform, move |name| {
            vars.iter().find(|(k, _)| k == name).map(|(_, v)| v.clone())
        })
    }

    fn programs(clipboard: Clipboard) -> Vec<&'static str> {
        clipboard.tools().iter().map(|t| t.program).collect()
    }

    #[test]
    fn each_platform_tries_its_own_tools() {
        assert_eq!(programs(clipboard(Platform::MacOs, &[])), ["pbcopy"]);
        assert_eq!(programs(clipboard(Platform::Windows, &[])), ["clip"]);
        assert_eq!(programs(clipboard(Platform::Other, &[])), ["xclip", "xsel"]);
        assert_eq!(
            programs(clipboard(
                Platform::Other,
                &[("WAYLAND_DISPLAY", "wayland-0")]
            )),
            ["wl-copy", "xclip", "xsel"]
        );
        assert_eq!(
            programs(clipboard(Platform::Other, &[("WSL_DISTRO_NAME", "Ubuntu")])),
            ["clip.exe"]
        );
        // Windows Terminal honours OSC 52, so WSL there uses it alone.
        let wt = clipboard(
            Platform::Other,
            &[("WSL_DISTRO_NAME", "Ubuntu"), ("WT_SESSION", "1")],
        );
        assert!(wt.tools().is_empty() && !wt.terminal_first());
    }

    #[test]
    fn over_ssh_the_terminal_is_asked_first_and_alone() {
        let ssh = clipboard(Platform::MacOs, &[("SSH_TTY", "/dev/pts/1")]);
        assert!(ssh.terminal_first() && ssh.tools().is_empty());
        // An empty variable is unset.
        assert!(!clipboard(Platform::MacOs, &[("SSH_TTY", "")]).terminal_first());
    }

    #[test]
    fn osc52_carries_the_text_in_base64() {
        assert_eq!(osc52("hi"), "\x1b]52;c;aGk=\x07");
        assert_eq!(base64(b""), "");
        assert_eq!(base64(b"f"), "Zg==");
        assert_eq!(base64(b"fo"), "Zm8=");
        assert_eq!(base64(b"foo"), "Zm9v");
        assert_eq!(base64("é漢".as_bytes()), "w6nmvKI=");
    }

    #[cfg(unix)]
    #[test]
    fn a_tool_succeeds_only_when_it_reads_its_input_and_exits_cleanly() {
        assert!(feed(tool("sh", &["-c", "cat >/dev/null"], false), "text"));
        assert!(!feed(tool("sh", &["-c", "exit 3"], false), "text"));
        assert!(!feed(
            tool("bake-no-such-clipboard-tool", &[], false),
            "text"
        ));
    }
}
