//! Puts selected text on the clipboard.
//!
//! Over SSH the terminal's own clipboard is the user's, so OSC 52 goes first
//! and no remote helper is run. Otherwise macOS uses `pbcopy`, Windows `clip`,
//! WSL (including Windows Terminal) `clip.exe`, Wayland `wl-copy`, and X11
//! `xclip` then `xsel`. When no tool succeeds, bounded OSC 52 is the fallback.

mod transport;

use std::io::Read;
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

use bake_tui_view::paste::{self, Image};

use self::transport::TerminalTransport;

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
    wayland: bool,
    transport: TerminalTransport,
}

impl Clipboard {
    pub fn new(platform: Platform, env: impl Fn(&str) -> Option<String>) -> Self {
        let set = |name: &str| env(name).is_some_and(|value| !value.is_empty());
        Self {
            platform,
            ssh: set("SSH_TTY") || set("SSH_CONNECTION") || set("SSH_CLIENT"),
            wsl: set("WSL_DISTRO_NAME") || set("WSL_INTEROP"),
            wayland: set("WAYLAND_DISPLAY"),
            transport: TerminalTransport::detect(&env),
        }
    }

    /// Whether the terminal itself is asked first, over SSH.
    pub fn terminal_first(self) -> bool {
        self.ssh
    }

    /// Encodes `text` as an OSC 52 sequence framed for this terminal, or
    /// returns `None` when the terminal does not support OSC 52 or `text`
    /// exceeds the safe OSC 52 payload cap.
    pub fn terminal_copy(self, text: &str) -> Option<String> {
        self.transport.encode(text)
    }

    /// The tools to try, in order, before the terminal's own clipboard.
    pub fn tools(self) -> Vec<Tool> {
        if self.ssh {
            return Vec::new();
        }
        match self.platform {
            Platform::MacOs => vec![tool("pbcopy", &[], false)],
            Platform::Windows => vec![tool("clip", &[], true)],
            Platform::Other if self.wsl => vec![tool("clip.exe", &[], true)],
            Platform::Other => {
                let mut tools = Vec::new();
                if self.wayland {
                    // Without a type wl-copy sniffs the contents: a copied JSON or
                    // HTML snippet might not be offered as text to another editor.
                    tools.push(tool(
                        "wl-copy",
                        &["--type", "text/plain;charset=utf-8"],
                        false,
                    ));
                }
                tools.push(tool("xclip", &["-selection", "clipboard"], false));
                tools.push(tool("xsel", &["--clipboard", "--input"], false));
                tools
            }
        }
    }
}

/// Image types the clipboard is asked for, in order of preference.
const IMAGE_TYPES: &[&str] = &["image/png", "image/jpeg", "image/webp", "image/gif"];

/// Largest clipboard image read.
const MAX_IMAGE: usize = 64 * 1024 * 1024;

impl Clipboard {
    /// Reads the clipboard's image, if it holds one: `wl-paste` on Wayland,
    /// then `xclip`; AppleScript writing PNG data to a private file on macOS;
    /// and PowerShell's clipboard bitmap as PNG on Windows. A missing tool
    /// reads as no image.
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
            data: paste::ImageData::new(data),
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

/// Reads the image a pasted path names: its type and size from its header,
/// and its bytes, up to [`MAX_IMAGE`]. `~/` is the home directory. `None`
/// when the file cannot be read, is larger than that, or is not an image
/// the composer stages.
pub fn read_image_file(path: &str, home: Option<&str>) -> Option<Image> {
    let path = match (path.strip_prefix("~/"), home) {
        (Some(rest), Some(home)) => Path::new(home).join(rest),
        _ => Path::new(path).to_path_buf(),
    };
    let file = std::fs::File::open(&path).ok()?;
    file.metadata().ok().filter(|m| m.is_file())?;
    let mut data = Vec::new();
    file.take(MAX_IMAGE as u64 + 1)
        .read_to_end(&mut data)
        .ok()?;
    if data.len() > MAX_IMAGE {
        return None;
    }
    let media_type = paste::sniff(&data)?;
    Some(Image {
        name: path.file_name()?.to_string_lossy().into_owned(),
        media_type,
        bytes: data.len() as u64,
        size: paste::pixel_size(&data),
        data: paste::ImageData::new(data),
    })
}

/// Runs `tool` with `text` on its standard input. Returns whether every byte
/// was written and the tool exited cleanly within [`FEED`]; a missing tool,
/// a truncated write, a failure, or a hang is `false`, and a hung tool is
/// killed.
pub fn feed(tool: Tool, text: &str) -> bool {
    crate::clipboard_feed::feed(tool, text, FEED)
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
        assert_eq!(
            programs(clipboard(
                Platform::Other,
                &[("WSL_INTEROP", "/run/WSL/1_interop")]
            )),
            ["clip.exe"]
        );
        // WSL under Windows Terminal still prefers clip.exe locally for
        // confirmation, Unicode, and selections larger than the OSC 52 cap.
        let wt = clipboard(
            Platform::Other,
            &[("WSL_DISTRO_NAME", "Ubuntu"), ("WT_SESSION", "1")],
        );
        assert_eq!(programs(wt), ["clip.exe"]);
        assert!(wt.tools()[0].utf16);
        assert!(!wt.terminal_first());
        assert_eq!(
            wt.terminal_copy("hi").as_deref(),
            Some("\x1b]52;c;aGk=\x07")
        );
    }

    #[test]
    fn over_ssh_the_terminal_is_asked_first_and_alone() {
        for var in ["SSH_TTY", "SSH_CONNECTION", "SSH_CLIENT"] {
            for platform in [Platform::MacOs, Platform::Windows, Platform::Other] {
                let ssh = clipboard(
                    platform,
                    &[
                        (var, "1"),
                        ("WSL_DISTRO_NAME", "Ubuntu"),
                        ("WAYLAND_DISPLAY", "wayland-0"),
                        ("WT_SESSION", "1"),
                    ],
                );
                assert!(ssh.terminal_first(), "{var} on {platform:?}");
                assert!(ssh.tools().is_empty(), "{var} on {platform:?}");
            }
            // An empty variable is unset.
            assert!(!clipboard(Platform::MacOs, &[(var, "")]).terminal_first());
        }
    }

    #[test]
    fn terminal_copy_routes_by_multiplexer_and_enforces_limits() {
        let plain = clipboard(Platform::Other, &[("SSH_CLIENT", "1.2.3.4 5 22")]);
        assert_eq!(
            plain.terminal_copy("hi").as_deref(),
            Some("\x1b]52;c;aGk=\x07")
        );

        let tmux = clipboard(
            Platform::Other,
            &[
                ("SSH_TTY", "/dev/pts/1"),
                ("TMUX", "/tmp/tmux-1000/default,1,0"),
            ],
        );
        assert_eq!(
            tmux.terminal_copy("hi").as_deref(),
            Some("\x1b]52;c;aGk=\x07")
        );

        let screen = clipboard(
            Platform::Other,
            &[("SSH_TTY", "/dev/pts/1"), ("STY", "123.pts-0.host")],
        );
        assert_eq!(
            screen.terminal_copy("hi").as_deref(),
            Some("\x1bP\x1b]52;c;aGk=\x07\x1b\\")
        );

        let nested = clipboard(
            Platform::Other,
            &[("TMUX", "/tmp/t"), ("STY", "123.pts-0.host")],
        );
        assert_eq!(
            nested.terminal_copy("hi").as_deref(),
            Some("\x1b]52;c;aGk=\x07")
        );

        let dumb = clipboard(
            Platform::Other,
            &[("SSH_TTY", "/dev/pts/1"), ("TERM", "dumb")],
        );
        assert_eq!(dumb.terminal_copy("hi"), None);

        let oversized = "a".repeat(transport::MAX_OSC52_RAW_BYTES + 1);
        assert_eq!(plain.terminal_copy(&oversized), None);
        assert_eq!(screen.terminal_copy(&oversized), None);
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
