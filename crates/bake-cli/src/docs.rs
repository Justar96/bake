//! `bake docs`: Bake's 0.4 user documentation, compiled into the binary
//! so it always describes the installed version and needs no files,
//! network, or Bake home. Each topic is a Markdown file in `docs/` and is
//! printed as written.

use std::path::Path;

/// One documentation topic.
pub struct Topic {
    /// The name `bake docs <name>` takes.
    pub name: &'static str,
    /// One line for the topic list.
    pub summary: &'static str,
    /// The Markdown text.
    pub text: &'static str,
}

/// Every topic, in the order the list shows them.
pub const TOPICS: &[Topic] = &[
    Topic {
        name: "cli",
        summary: "Running prompts, output modes, tool flags, and exit statuses",
        text: include_str!("../docs/cli.md"),
    },
    Topic {
        name: "home",
        summary: "The Bake home, what it holds, and what is read from Bake 0.3",
        text: include_str!("../docs/home.md"),
    },
    Topic {
        name: "models",
        summary: "Choosing a model, models.json, auth.json, and the CLIProxyAPI route",
        text: include_str!("../docs/models.md"),
    },
    Topic {
        name: "settings",
        summary: "The members of settings.json",
        text: include_str!("../docs/settings.md"),
    },
    Topic {
        name: "permissions",
        summary: "Permission presets, sandbox modes, and approvals",
        text: include_str!("../docs/permissions.md"),
    },
    Topic {
        name: "sessions",
        summary: "Where sessions are saved and how to continue one",
        text: include_str!("../docs/sessions.md"),
    },
    Topic {
        name: "context",
        summary: "AGENTS.md instruction files",
        text: include_str!("../docs/context.md"),
    },
    Topic {
        name: "plugins",
        summary: "Wasm plugins that veto tool calls: install, enable, disable",
        text: include_str!("../docs/plugins.md"),
    },
    Topic {
        name: "skills",
        summary: "Agent Skills, where they are found, and /skill:<name>",
        text: include_str!("../docs/skills.md"),
    },
    Topic {
        name: "mcp",
        summary: "MCP servers from mcp.json, their tools, and --no-mcp",
        text: include_str!("../docs/mcp.md"),
    },
    Topic {
        name: "codemode",
        summary: "The codemode tool: scripts that call the other tools",
        text: include_str!("../docs/codemode.md"),
    },
    Topic {
        name: "rpc",
        summary: "--mode rpc: the JSONL protocol, its commands, and approvals",
        text: include_str!("../docs/rpc.md"),
    },
    Topic {
        name: "hooks",
        summary: "Claude Code and Codex hooks.json command hooks",
        text: include_str!("../docs/hooks.md"),
    },
    Topic {
        name: "update",
        summary: "bake update: installing a published release, and rolling back",
        text: include_str!("../docs/update.md"),
    },
];

/// The topic named `name`.
pub fn topic(name: &str) -> Option<&'static Topic> {
    TOPICS.iter().find(|topic| topic.name == name)
}

/// The topic list `bake docs` prints.
pub fn topic_list() -> String {
    let width = TOPICS
        .iter()
        .map(|topic| topic.name.len())
        .max()
        .unwrap_or(0);
    let mut list = String::from("Bake documentation topics:\n\n");
    for topic in TOPICS {
        list.push_str(&format!(
            "  {:width$}  {}\n",
            topic.name,
            topic.summary,
            width = width
        ));
    }
    list.push_str("\nRead one with: bake docs <topic>\n");
    list
}

/// How to run `bake docs` through `exe`, as a POSIX shell command: the
/// model runs it with the bash tool, which is Git Bash on Windows. Windows
/// separators become `/`, as in the prompt's working directory, and a path
/// holding anything but letters, digits, and `-_./:,+=@%` is single-quoted.
/// A plain path also runs in PowerShell; a quoted one needs `& ` there. A
/// path that is not UTF-8 cannot be named, so it gives `bake docs`.
pub fn docs_command(exe: &Path) -> String {
    let Some(path) = exe.to_str() else {
        return "bake docs".to_owned();
    };
    let path = if cfg!(windows) {
        path.replace('\\', "/")
    } else {
        path.to_owned()
    };
    let plain = !path.is_empty()
        && path
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "-_./:,+=@%".contains(c));
    if plain {
        format!("{path} docs")
    } else {
        format!("'{}' docs", path.replace('\'', r"'\''"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_topic_starts_with_a_title_and_a_summary_paragraph() {
        for topic in TOPICS {
            let mut lines = topic.text.lines();
            let title = lines.next().unwrap_or_default();
            assert!(title.starts_with("# "), "{} has no title", topic.name);
            assert_eq!(lines.next(), Some(""), "{}", topic.name);
            let summary = lines.next().unwrap_or_default();
            assert!(
                !summary.is_empty() && !summary.starts_with('#'),
                "{} has no summary paragraph",
                topic.name
            );
            assert!(topic.text.ends_with('\n') && !topic.text.ends_with("\n\n"));
        }
    }

    #[test]
    fn topic_names_are_unique_and_listed() {
        let list = topic_list();
        for (index, topic) in TOPICS.iter().enumerate() {
            assert!(
                TOPICS[index + 1..]
                    .iter()
                    .all(|other| other.name != topic.name),
                "{} is listed twice",
                topic.name
            );
            assert!(list.contains(topic.name) && list.contains(topic.summary));
            assert_eq!(
                super::topic(topic.name).map(|found| found.text),
                Some(topic.text)
            );
        }
        assert!(super::topic("nope").is_none());
    }

    #[test]
    fn topics_name_only_topics_that_exist() {
        for topic in TOPICS {
            for (index, _) in topic.text.match_indices("bake docs ") {
                let rest = &topic.text[index + "bake docs ".len()..];
                let name: String = rest
                    .chars()
                    .take_while(|c| c.is_ascii_lowercase())
                    .collect();
                if !name.is_empty() {
                    assert!(
                        super::topic(&name).is_some(),
                        "{} names a missing topic {name}",
                        topic.name
                    );
                }
            }
        }
    }

    #[test]
    fn plain_paths_are_not_quoted() {
        assert_eq!(
            docs_command(Path::new("/usr/local/bin/bake")),
            "/usr/local/bin/bake docs"
        );
        assert_eq!(docs_command(Path::new("bake")), "bake docs");
    }

    #[test]
    fn other_paths_are_single_quoted() {
        assert_eq!(
            docs_command(Path::new("/opt/My Apps/bake")),
            "'/opt/My Apps/bake' docs"
        );
        assert_eq!(
            docs_command(Path::new("/opt/it's/$x/bake")),
            r"'/opt/it'\''s/$x/bake' docs"
        );
        assert_eq!(docs_command(Path::new("")), "'' docs");
    }

    #[cfg(unix)]
    #[test]
    fn a_path_that_is_not_utf8_gives_the_bare_command() {
        use std::os::unix::ffi::OsStrExt;
        let exe = std::ffi::OsStr::from_bytes(b"/opt/\xff/bake");
        assert_eq!(docs_command(Path::new(exe)), "bake docs");
    }

    // Git Bash would drop unquoted backslashes.
    #[cfg(windows)]
    #[test]
    fn windows_paths_use_forward_slashes() {
        assert_eq!(
            docs_command(Path::new(r"C:\Tools\bake.exe")),
            "C:/Tools/bake.exe docs"
        );
        assert_eq!(
            docs_command(Path::new(r"C:\Program Files\Bake\bake.exe")),
            "'C:/Program Files/Bake/bake.exe' docs"
        );
    }
}
