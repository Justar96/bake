//! Puts selected text on the clipboard, as the TypeScript
//! `writeClipboardText` does.
//!
//! Over SSH the terminal's own clipboard is the user's, so OSC 52 goes first.
//! Otherwise macOS uses `pbcopy`, Windows `clip`, Wayland `wl-copy`, and X11
//! `xclip` then `xsel`. WSL uses OSC 52 under Windows Terminal, which honours
//! it, and `clip.exe` elsewhere. When no tool succeeds, OSC 52 is the last
//! resort; a terminal that ignores it gives no sign, so it counts as copied.

use std::io::{Read, Write};
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

use bake_tui_view::paste::{self, Image};

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

/// Image types the clipboard is asked for, in the oracle's preference order.
const IMAGE_TYPES: &[&str] = &["image/png", "image/jpeg", "image/webp", "image/gif"];

/// Largest clipboard image read; the oracle's attachment admission refuses
/// larger ones anyway.
const MAX_IMAGE: usize = 64 * 1024 * 1024;

impl Clipboard {
    /// Reads the clipboard's image, if it holds one, as the TypeScript
    /// `readClipboardImage` does: `wl-paste` on Wayland, then `xclip`;
    /// AppleScript writing PNG data to a private file on macOS; and
    /// PowerShell's clipboard bitmap as PNG on Windows. A missing tool reads
    /// as no image.
    pub fn read_image(self) -> Option<Image> {
        let (data, media_type) = match self.platform {
            Platform::MacOs => (read_mac_image()?, "image/png"),
            Platform::Windows => (
                run(
                    "powershell",
                    &["-NoProfile", "-NonInteractive", "-Command", WINDOWS_IMAGE],
                )?,
                "image/png",
            ),
            Platform::Other => {
                let wayland = self
                    .wayland
                    .then(|| {
                        let listing = run("wl-paste", &["--list-types"])?;
                        let kind = image_type(&String::from_utf8_lossy(&listing))?;
                        Some((run("wl-paste", &["--no-newline", "--type", kind])?, kind))
                    })
                    .flatten();
                match wayland {
                    Some(image) => image,
                    None => {
                        let listing =
                            run("xclip", &["-selection", "clipboard", "-t", "TARGETS", "-o"])?;
                        let kind = image_type(&String::from_utf8_lossy(&listing))?;
                        (
                            run("xclip", &["-selection", "clipboard", "-t", kind, "-o"])?,
                            kind,
                        )
                    }
                }
            }
        };
        if data.is_empty() {
            return None;
        }
        let media_type = paste::sniff(&data).unwrap_or(media_type);
        Some(Image {
            name: format!("clipboard.{}", &media_type["image/".len()..]),
            media_type,
            bytes: data.len() as u64,
            size: paste::pixel_size(&data),
        })
    }
}

const WINDOWS_IMAGE: &str = "Add-Type -AssemblyName System.Windows.Forms,System.Drawing; \
    $i = [Windows.Forms.Clipboard]::GetImage(); \
    if ($i) { $m = New-Object IO.MemoryStream; $i.Save($m, [Drawing.Imaging.ImageFormat]::Png); \
    $o = [Console]::OpenStandardOutput(); $o.Write($m.ToArray(), 0, $m.Length); $o.Flush() }";

/// The first image type a clipboard lists, in [`IMAGE_TYPES`] order.
pub fn image_type(listing: &str) -> Option<&'static str> {
    let offered: Vec<String> = listing
        .lines()
        .map(|line| line.trim().to_ascii_lowercase())
        .collect();
    IMAGE_TYPES
        .iter()
        .copied()
        .find(|kind| offered.iter().any(|line| line == kind))
}

/// Asks AppleScript for the clipboard as PNG, written to a private file that
/// is removed after it is read.
fn read_mac_image() -> Option<Vec<u8>> {
    let directory = std::env::temp_dir().join(format!("bake-clipboard-{}", std::process::id()));
    std::fs::create_dir(&directory).ok()?;
    let file = directory.join("clipboard.png");
    let script = format!(
        "set f to open for access POSIX file \"{}\" with write permission",
        file.display()
    );
    let ran = run(
        "osascript",
        &[
            "-e",
            "set png to (the clipboard as «class PNGf»)",
            "-e",
            &script,
            "-e",
            "write png to f",
            "-e",
            "close access f",
        ],
    );
    let data = ran.and_then(|_| std::fs::read(&file).ok());
    let _ = std::fs::remove_dir_all(&directory);
    data
}

/// Runs `program` and returns its standard output, at most [`MAX_IMAGE`]
/// bytes, when it exits successfully within [`FEED`]; a hung program is
/// killed.
fn run(program: &str, args: &[&str]) -> Option<Vec<u8>> {
    let mut child = Command::new(program)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let stdout = child.stdout.take()?;
    let (sent, output) = mpsc::channel();
    // Read on its own thread so a full pipe never stalls the wait below.
    let reader = thread::spawn(move || {
        let mut data = Vec::new();
        let ok = stdout
            .take(MAX_IMAGE as u64 + 1)
            .read_to_end(&mut data)
            .is_ok();
        let _ = sent.send(ok.then_some(data));
    });
    let deadline = Instant::now() + FEED;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Some(status),
            Ok(None) if Instant::now() < deadline => thread::sleep(POLL),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                break None;
            }
        }
    };
    let _ = reader.join();
    let data = output.recv().ok().flatten()?;
    (status?.success() && data.len() <= MAX_IMAGE).then_some(data)
}

/// Reads the image a pasted path names: its header for the type and size,
/// and its length. `~/` is the home directory. `None` when the file cannot
/// be read or is not an image the preview stages.
pub fn read_image_file(path: &str, home: Option<&str>) -> Option<Image> {
    let path = match (path.strip_prefix("~/"), home) {
        (Some(rest), Some(home)) => Path::new(home).join(rest),
        _ => Path::new(path).to_path_buf(),
    };
    let mut file = std::fs::File::open(&path).ok()?;
    let bytes = file.metadata().ok().filter(|m| m.is_file())?.len();
    let mut header = [0; 32];
    let read = file.read(&mut header).ok()?;
    let header = &header[..read];
    let media_type = paste::sniff(header)?;
    Some(Image {
        name: path.file_name()?.to_string_lossy().into_owned(),
        media_type,
        bytes,
        size: paste::pixel_size(header),
    })
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

    #[test]
    fn the_first_image_type_listed_in_preference_order_is_asked_for() {
        assert_eq!(
            image_type("TARGETS\nimage/gif\nimage/png\n"),
            Some("image/png")
        );
        assert_eq!(image_type(" IMAGE/JPEG \r\ntext/plain"), Some("image/jpeg"));
        assert_eq!(image_type("text/plain\nUTF8_STRING"), None);
    }

    #[test]
    fn a_pasted_path_reads_as_an_image_by_its_header() {
        let dir = std::env::temp_dir().join(format!("bake-image-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let mut png = b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR".to_vec();
        png.extend(3u32.to_be_bytes());
        png.extend(2u32.to_be_bytes());
        png.extend([0; 40]);
        std::fs::write(dir.join("shot.png"), &png).unwrap();
        std::fs::write(dir.join("notes.png"), b"not an image").unwrap();
        let image = read_image_file(&dir.join("shot.png").to_string_lossy(), None).unwrap();
        assert_eq!(
            image.summary(),
            format!("shot.png · image/png · {} B · 3×2", png.len())
        );
        // A home-relative path, a non-image, and a missing file.
        let home = dir.to_string_lossy().into_owned();
        assert!(read_image_file("~/shot.png", Some(&home)).is_some());
        assert!(read_image_file(&dir.join("notes.png").to_string_lossy(), None).is_none());
        assert!(read_image_file(&dir.join("gone.png").to_string_lossy(), None).is_none());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn a_program_output_is_read_only_when_it_exits_cleanly() {
        assert_eq!(
            run("sh", &["-c", "printf abc"]).as_deref(),
            Some(&b"abc"[..])
        );
        assert_eq!(run("sh", &["-c", "printf abc; exit 1"]), None);
        assert_eq!(run("bake-no-such-clipboard-tool", &[]), None);
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
