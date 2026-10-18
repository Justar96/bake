//! The owner rules of `$BAKE_HOME/mcp.json`. Each layout is built in a
//! private temporary directory whose modes the test sets, so the host's
//! umask decides nothing.

use std::path::{Path, PathBuf};

use super::*;
#[cfg(unix)]
use crate::mcp::config::MAX_CONFIG_BYTES;
use crate::test_support::TempDir;

const ONE_SERVER: &str = r#"{"mcpServers":{"files":{"command":"mcp-files"}}}"#;

/// `root/user` (the user's home), `root/work` (the session's working
/// directory), and `root/home` (the Bake home), each `0755` on Unix.
struct Layout {
    root: TempDir,
}

impl Layout {
    fn new(name: &str) -> Self {
        let root = TempDir::new(name);
        for dir in ["user", "work", "home"] {
            std::fs::create_dir_all(root.join(dir)).expect("layout directory");
        }
        let layout = Self { root };
        for dir in ["", "user", "work", "home"] {
            layout.chmod(&layout.root.path().join(dir), 0o755);
        }
        layout
    }

    fn path(&self, name: &str) -> PathBuf {
        self.root.join(name)
    }

    fn home(&self) -> PathBuf {
        self.path("home")
    }

    fn write_config(&self, home: &Path, text: &str) -> PathBuf {
        std::fs::create_dir_all(home).expect("home");
        self.chmod(home, 0o755);
        let path = home.join(MCP_CONFIG_FILE);
        std::fs::write(&path, text).expect("mcp.json");
        self.chmod(&path, 0o644);
        path
    }

    fn load(&self, home: &Path) -> McpConfig {
        load_home_mcp_config(home, &self.path("work"), Some(&self.path("user")))
    }

    #[cfg(unix)]
    fn chmod(&self, path: &Path, mode: u32) {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).expect("chmod");
    }

    #[cfg(not(unix))]
    fn chmod(&self, _path: &Path, _mode: u32) {}
}

/// The one error a refused file gives, which also says nothing started.
fn refusal(config: &McpConfig) -> &str {
    assert!(config.servers.is_empty(), "{config:?}");
    assert_eq!(config.errors.len(), 1, "{config:?}");
    let error = &config.errors[0];
    assert!(
        error.ends_with("No MCP server was started from it"),
        "{error}"
    );
    error
}

#[test]
fn a_missing_file_or_home_is_an_empty_configuration() {
    let layout = Layout::new("mcp-home-missing");
    assert_eq!(layout.load(&layout.home()), McpConfig::default());
    assert_eq!(
        layout.load(&layout.path("no-such-home")),
        McpConfig::default()
    );
}

#[cfg(unix)]
#[test]
fn the_owners_file_loads_with_its_entry_errors() {
    let layout = Layout::new("mcp-home-valid");
    let path = layout.write_config(
        &layout.home(),
        r#"{"mcpServers":{"files":{"command":"mcp-files"},"bad":{"args":[]}}}"#,
    );
    let config = layout.load(&layout.home());
    assert_eq!(config.servers.len(), 1);
    assert_eq!(config.servers[0].name, "files");
    // Per-entry errors are kept for the caller to report, as Pi's are.
    assert_eq!(config.errors.len(), 1);
    assert!(
        config.errors[0].starts_with(&format!("{}: server \"bad\"", path.display())),
        "{:?}",
        config.errors
    );
}

#[cfg(unix)]
#[test]
fn a_directory_or_an_oversized_file_is_refused() {
    let layout = Layout::new("mcp-home-shape");
    std::fs::create_dir_all(layout.home().join(MCP_CONFIG_FILE)).expect("directory");
    assert!(refusal(&layout.load(&layout.home())).contains("is not a regular file"));
    let other = layout.path("big");
    layout.write_config(&other, &" ".repeat(MAX_CONFIG_BYTES as usize + 1));
    assert!(refusal(&layout.load(&other)).contains("larger than"));
}

// A repository, or the agent working in it, could otherwise supply the
// servers and the credentials their `env` passes.
#[cfg(unix)]
#[test]
fn a_file_in_the_workspace_or_a_work_tree_is_refused() {
    let layout = Layout::new("mcp-home-workspace");
    let inside = layout.path("work").join(".bake");
    layout.write_config(&inside, ONE_SERVER);
    let error = refusal(&layout.load(&inside)).to_owned();
    assert!(
        error.contains("inside the session's working directory"),
        "{error}"
    );
    assert!(error.contains("--no-mcp"), "{error}");

    let repo = layout.path("repo");
    std::fs::create_dir_all(repo.join(".git")).expect(".git");
    layout.chmod(&repo, 0o755);
    let nested = repo.join("sub").join(".bake");
    std::fs::create_dir_all(&nested).expect("nested home");
    layout.chmod(&repo.join("sub"), 0o755);
    layout.chmod(&nested, 0o755);
    layout.write_config(&nested, ONE_SERVER);
    let error = refusal(&layout.load(&nested)).to_owned();
    assert!(error.contains("inside the git work tree"), "{error}");
    assert!(
        error.contains(
            &std::fs::canonicalize(&repo)
                .expect("repo")
                .display()
                .to_string()
        ),
        "{error}"
    );
}

// The user's home directory and the directories above it are the user's
// own: running there, or keeping the home in git, refuses nothing.
#[cfg(unix)]
#[test]
fn the_users_home_directory_is_not_a_workspace_or_repository() {
    let layout = Layout::new("mcp-home-user");
    let user = layout.path("user");
    std::fs::create_dir_all(user.join(".git")).expect("dotfiles repository");
    let home = user.join(".bake");
    std::fs::create_dir_all(&home).expect("home");
    layout.chmod(&home, 0o700);
    layout.write_config(&home, ONE_SERVER);
    let config = load_home_mcp_config(&home, &user, Some(&user));
    assert!(config.errors.is_empty(), "{:?}", config.errors);
    assert_eq!(config.servers.len(), 1);
    // Without a known user home, every directory counts.
    let unknown = load_home_mcp_config(&home, &layout.path("work"), None);
    assert!(refusal(&unknown).contains("inside the git work tree"));
}

// Where who may change the file cannot be shown, an existing file is
// refused unread; a missing one is still an ordinary startup.
#[cfg(not(unix))]
#[test]
fn an_existing_file_is_refused_where_its_owner_cannot_be_checked() {
    let layout = Layout::new("mcp-home-unchecked");
    layout.write_config(&layout.home(), ONE_SERVER);
    let error = refusal(&layout.load(&layout.home())).to_owned();
    assert!(error.contains(super::UNCHECKED_PLATFORM), "{error}");
}

#[cfg(unix)]
mod unix {
    use std::time::Duration;

    use super::*;

    #[test]
    fn a_linked_file_is_refused_and_its_target_never_read() {
        let layout = Layout::new("mcp-home-link");
        let target = layout.write_config(&layout.path("elsewhere"), ONE_SERVER);
        std::os::unix::fs::symlink(&target, layout.home().join(MCP_CONFIG_FILE)).expect("link");
        assert!(refusal(&layout.load(&layout.home())).contains("is a symbolic link"));
    }

    // The name is not trusted: a home that is a link is judged where it
    // leads.
    #[test]
    fn a_linked_home_is_judged_where_it_leads() {
        let layout = Layout::new("mcp-home-linked-home");
        let real = layout.path("work").join("real-home");
        layout.write_config(&real, ONE_SERVER);
        let home = layout.path("linked-home");
        std::os::unix::fs::symlink(&real, &home).expect("link");
        assert!(refusal(&layout.load(&home)).contains("inside the session's working directory"));
    }

    // `open` on a FIFO blocks until a writer opens it; the read must not.
    #[test]
    fn a_fifo_is_refused_without_waiting_for_a_writer() {
        let layout = Layout::new("mcp-home-fifo");
        let fifo = layout.home().join(MCP_CONFIG_FILE);
        let name = std::ffi::CString::new(fifo.as_os_str().as_encoded_bytes()).expect("path");
        // SAFETY: a NUL-terminated path and a mode.
        assert_eq!(unsafe { libc::mkfifo(name.as_ptr(), 0o600) }, 0);
        let (send, receive) = std::sync::mpsc::channel();
        let home = layout.home();
        let work = layout.path("work");
        // Detached, so a regression fails the test instead of hanging it.
        std::thread::spawn(move || {
            let _ = send.send(load_home_mcp_config(&home, &work, None));
        });
        let config = receive
            .recv_timeout(Duration::from_secs(10))
            .expect("the read returned without a writer");
        assert!(refusal(&config).contains("is not a regular file"));
    }

    #[test]
    fn a_file_others_can_change_is_refused() {
        let layout = Layout::new("mcp-home-mode");
        let path = layout.write_config(&layout.home(), ONE_SERVER);
        for mode in [0o664, 0o646, 0o666] {
            layout.chmod(&path, mode);
            let error = refusal(&layout.load(&layout.home())).to_owned();
            assert!(
                error.contains(&format!("mode {mode:04o}")) && error.contains("chmod go-w"),
                "{error}"
            );
        }
        // Readable by others is not refused; only writing is.
        layout.chmod(&path, 0o644);
        assert!(layout.load(&layout.home()).errors.is_empty());
        layout.chmod(&path, 0o600);
        assert!(layout.load(&layout.home()).errors.is_empty());
    }

    #[test]
    fn a_directory_others_can_change_is_refused_unless_sticky() {
        let layout = Layout::new("mcp-home-dir");
        layout.write_config(&layout.home(), ONE_SERVER);
        for mode in [0o775, 0o757, 0o777] {
            layout.chmod(&layout.home(), mode);
            let error = refusal(&layout.load(&layout.home())).to_owned();
            assert!(
                error.contains("its directory") && error.contains(&format!("mode {mode:04o}")),
                "{error}"
            );
        }
        // An ancestor counts as the home itself does.
        layout.chmod(&layout.home(), 0o700);
        layout.chmod(layout.root.path(), 0o777);
        assert!(refusal(&layout.load(&layout.home())).contains("its directory"));
        // In a sticky directory only an entry's owner may rename or remove
        // it, as in `/tmp`.
        layout.chmod(layout.root.path(), 0o1777);
        assert!(layout.load(&layout.home()).errors.is_empty());
        layout.chmod(layout.root.path(), 0o755);
        assert!(layout.load(&layout.home()).errors.is_empty());
    }

    // The spelled path is resolved once and never opened: a link in it
    // swapped afterwards, toward the workspace, changes nothing read.
    #[test]
    fn a_link_in_the_spelled_path_swapped_after_resolution_reaches_nothing() {
        let layout = Layout::new("mcp-home-swap-link");
        let trusted = layout.path("trusted");
        layout.write_config(&trusted.join("home"), ONE_SERVER);
        layout.chmod(&trusted, 0o755);
        let evil = layout.path("work").join("evil");
        layout.write_config(
            &evil.join("home"),
            r#"{"mcpServers":{"evil":{"command":"evil"}}}"#,
        );
        let link = layout.path("work").join("link");
        std::os::unix::fs::symlink(&trusted, &link).expect("link");
        let home = link.join("home");
        let resolved = std::fs::canonicalize(&home).expect("resolved");
        {
            let (link, evil) = (link.clone(), evil.clone());
            seam::on_after_resolve(&resolved, move || {
                std::fs::remove_file(&link).expect("unlink");
                std::os::unix::fs::symlink(&evil, &link).expect("relink");
            });
        }
        let config = layout.load(&home);
        assert!(config.errors.is_empty(), "{:?}", config.errors);
        let names: Vec<_> = config.servers.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, ["files"]);
        // The swap happened: the spelled path now leads to the workspace.
        assert_eq!(
            std::fs::canonicalize(&home).expect("now"),
            std::fs::canonicalize(evil.join("home")).expect("evil")
        );
    }

    // A component of the resolved path replaced by a link after resolution
    // fails the anchored open; nothing behind the link is read.
    #[test]
    fn a_resolved_directory_replaced_by_a_link_is_refused() {
        let layout = Layout::new("mcp-home-swap-dir");
        layout.write_config(&layout.home(), ONE_SERVER);
        let evil = layout.path("work").join("evil");
        layout.write_config(&evil, r#"{"mcpServers":{"evil":{"command":"evil"}}}"#);
        let home = layout.home();
        let resolved = std::fs::canonicalize(&home).expect("resolved");
        {
            let (home, evil, aside) = (home.clone(), evil.clone(), layout.path("aside"));
            seam::on_after_resolve(&resolved, move || {
                std::fs::rename(&home, &aside).expect("move aside");
                std::os::unix::fs::symlink(&evil, &home).expect("link in place");
            });
        }
        let error = refusal(&layout.load(&home)).to_owned();
        assert!(
            error.contains("changed after bake-rs resolved the Bake home"),
            "{error}"
        );
    }

    // The same with the file itself: a link swapped in for `mcp.json` is
    // refused, not followed.
    #[test]
    fn a_file_replaced_by_a_link_after_resolution_is_refused() {
        let layout = Layout::new("mcp-home-swap-file");
        let file = layout.write_config(&layout.home(), ONE_SERVER);
        let evil = layout.write_config(
            &layout.path("work").join("evil"),
            r#"{"mcpServers":{"evil":{"command":"evil"}}}"#,
        );
        let resolved = std::fs::canonicalize(layout.home()).expect("resolved");
        seam::on_after_resolve(&resolved, move || {
            std::fs::remove_file(&file).expect("remove");
            std::os::unix::fs::symlink(&evil, &file).expect("link");
        });
        assert!(refusal(&layout.load(&layout.home())).contains("is a symbolic link"));
    }

    /// `chmod +a <entry> <path>`, macOS's ACL editor.
    #[cfg(target_os = "macos")]
    fn add_acl(path: &Path, entry: &str) {
        let status = std::process::Command::new("/bin/chmod")
            .arg("+a")
            .arg(entry)
            .arg(path)
            .status()
            .expect("chmod +a");
        assert!(status.success(), "chmod +a {entry:?}");
    }

    #[cfg(target_os = "macos")]
    fn clear_acl(path: &Path) {
        let status = std::process::Command::new("/bin/chmod")
            .arg("-N")
            .arg(path)
            .status()
            .expect("chmod -N");
        assert!(status.success());
    }

    // macOS extended ACLs grant access the mode bits do not show: an
    // `allow` entry letting anyone but the owner change the file or a
    // directory above it refuses the file; `deny` entries and the owner's
    // own entries do not.
    #[cfg(target_os = "macos")]
    #[test]
    fn a_foreign_write_acl_is_refused_and_deny_entries_kept() {
        let layout = Layout::new("mcp-home-acl");
        let path = layout.write_config(&layout.home(), ONE_SERVER);
        let me = String::from_utf8(
            std::process::Command::new("/usr/bin/id")
                .arg("-un")
                .output()
                .expect("id -un")
                .stdout,
        )
        .expect("user name");
        let me = me.trim();

        add_acl(&path, "everyone deny write,delete");
        add_acl(&path, &format!("user:{me} allow write,append"));
        let config = layout.load(&layout.home());
        assert!(config.errors.is_empty(), "{:?}", config.errors);

        for entry in [
            "everyone allow write",
            "group:staff allow append",
            "everyone allow writesecurity",
        ] {
            clear_acl(&path);
            add_acl(&path, entry);
            let error = refusal(&layout.load(&layout.home())).to_owned();
            assert!(error.contains("access control entry"), "{entry}: {error}");
        }
        clear_acl(&path);
        assert!(layout.load(&layout.home()).errors.is_empty());

        // A directory above the file: another user could replace it.
        add_acl(&layout.home(), "everyone allow add_file,delete_child");
        let error = refusal(&layout.load(&layout.home())).to_owned();
        assert!(
            error.contains("its directory") && error.contains("access control entry"),
            "{error}"
        );
        clear_acl(&layout.home());
        add_acl(&layout.home(), "everyone deny delete");
        assert!(layout.load(&layout.home()).errors.is_empty());
    }
}

/// The macOS ACL check's reading of each libc result fails closed
/// (`acl_rules`), checked on every platform.
mod acl_rules {
    use super::super::acl_rules::{ACL_MAX_ENTRIES, CHANGES, Kind, Step, kind, perm_set, step};

    #[test]
    fn only_the_documented_end_ends_the_entries() {
        assert_eq!(step(0, 0, false, 0), Step::Entry);
        assert_eq!(step(-1, 22, true, 0), Step::End, "an empty ACL");
        assert_eq!(step(-1, 22, true, 3), Step::End);
        for errno in [0, 1, 12, 34] {
            assert_eq!(step(-1, errno, true, 3), Step::Unreadable, "errno {errno}");
        }
        assert_eq!(step(1, 0, false, 0), Step::Unreadable);
        assert_eq!(step(-2, 22, true, 0), Step::Unreadable);
        assert_eq!(step(0, 0, true, 0), Step::Unreadable, "a null entry");
        assert_eq!(
            step(0, 0, false, ACL_MAX_ENTRIES),
            Step::Unreadable,
            "more entries than an ACL holds"
        );
    }

    #[test]
    fn unknown_tags_and_permission_results_refuse() {
        assert_eq!(kind(1), Kind::Allow);
        assert_eq!(kind(2), Kind::Deny);
        for tag in [0, 3, -1, 0x100] {
            assert_eq!(kind(tag), Kind::Unknown, "tag {tag}");
        }
        assert_eq!(perm_set(1), Some(true));
        assert_eq!(perm_set(0), Some(false));
        assert_eq!(perm_set(-1), None);
        assert_eq!(perm_set(2), None);
        assert_eq!(CHANGES.len(), 8);
    }
}
