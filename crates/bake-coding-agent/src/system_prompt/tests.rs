//! Bake's prompt text, checked whole against `tests/fixtures/system_prompt/`,
//! and the section mechanics ported from Pi `test/system-prompt.test.ts`
//! (v1.1.0), except its skills and forced-prompt cases (not ported).

use super::*;

fn options(cwd: &str, tools: &[&str]) -> SystemPromptOptions {
    let mut options = SystemPromptOptions::new(cwd);
    options.selected_tools = tools.iter().map(|tool| (*tool).to_owned()).collect();
    options
}

fn prompt(options: &SystemPromptOptions) -> String {
    build_system_prompt(options).unwrap_or_else(|error| error.to_string())
}

/// A fixture's prompt, without the newline the file ends with.
fn fixture(text: &str) -> &str {
    text.strip_suffix('\n').unwrap_or(text)
}

fn pairs(entries: &[(&str, &str)]) -> Vec<(String, String)> {
    entries
        .iter()
        .map(|(key, value)| ((*key).to_owned(), (*value).to_owned()))
        .collect()
}

fn names(options: &SystemPromptOptions) -> Vec<String> {
    build_system_prompt_sections(options)
        .unwrap_or_default()
        .into_iter()
        .map(|(name, _)| name)
        .collect()
}

/// `bake -p` with Pi's four default tools and their guidelines
/// (`tools/{read,edit,write}.ts`; `bash` contributes none), the
/// `workspace-write` preset with no one to answer approvals, and the facts
/// of a Linux repository: the example in
/// `docs/roadmap/rust-0.4/system-prompt.md`.
fn default_options() -> SystemPromptOptions {
    let mut options = options("/home/user/project", &["read", "bash", "edit", "write"]);
    options.tool_guidelines = vec![
        ("read".into(), vec!["Use read to examine files instead of cat or sed.".into()]),
        (
            "edit".into(),
            vec![
                "Use edit for precise changes (edits[].oldText must match exactly)".into(),
                "When changing multiple separate locations in one file, use one edit call with multiple entries in edits[] instead of multiple edit calls".into(),
                "Each edits[].oldText is matched against the original file, not after earlier edits are applied. Do not emit overlapping or nested edits. Merge nearby changes into one edit.".into(),
                "Keep edits[].oldText as small as possible while still being unique in the file. Do not pad with large unchanged regions.".into(),
            ],
        ),
        ("write".into(), vec!["Use write only for new files or complete rewrites.".into()]),
    ];
    options.permissions = "File sandbox: workspace-write. Sandboxed tools can modify files under \"/home/user/project\", and possibly in some temporary directories.\n\nApproval prompts: disabled. Actions that need approval are rejected automatically, so do not request sandbox escalation with `sandbox_permissions`.".into();
    options.docs_command = "/usr/local/bin/bake docs".into();
    options.environment = Environment {
        platform: "linux".into(),
        shell: "bash".into(),
        git_repository: true,
        started: "2026-10-13".into(),
    };
    options
}

#[test]
fn default_prompt_matches_the_fixture() {
    assert_eq!(
        prompt(&default_options()),
        fixture(include_str!(
            "../../tests/fixtures/system_prompt/default.txt"
        ))
    );
    assert_eq!(
        names(&default_options()),
        [
            "preamble",
            "rules",
            "docs",
            "run",
            "permissions",
            "environment"
        ]
    );
}

#[test]
fn custom_prompt_matches_the_fixture() {
    let mut custom = default_options();
    custom.custom_prompt = Some("Custom preamble.".into());
    custom.append_system_prompt = "Appended.".into();
    custom.sections = pairs(&[("notes", "N"), ("environment", "override")]);
    custom.context_files = vec![ContextFile {
        path: "a".into(),
        content: "b".into(),
    }];
    assert_eq!(
        prompt(&custom),
        fixture(include_str!(
            "../../tests/fixtures/system_prompt/custom.txt"
        ))
    );
}

// The rules a session without tools keeps, and the facts it states.
#[test]
fn no_tools() {
    let mut bare = default_options();
    bare.selected_tools = Vec::new();
    let text = prompt(&bare);
    assert!(text.contains(
        "<rules>\n- Follow the conventions of the surrounding code and the project's instructions."
    ));
    assert!(text.contains(
        "The user's requests take precedence over project instructions.\n- Commit or push only when the task requests it, and do not discard changes you did not make.\n- Report results and verification accurately, including failures and skipped checks.\n- Keep the final answer concise, factual, and neutral.\n</rules>"
    ));
    assert!(!text.contains("round trips"));
    assert!(!text.contains("Shell:"));
    assert!(text.ends_with(
        "<environment>\nWorking directory: /home/user/project\nPlatform: linux\nGit repository: yes\nSession started: 2026-10-13 (UTC)\n</environment>"
    ));
}

#[test]
fn environment_states_its_facts() {
    let mut windows = options("C:\\work\\project", &["read"]);
    windows.environment.platform = "windows".into();
    windows.environment.shell = "bash".into();
    let text = prompt(&windows);
    assert!(text.ends_with(
        "<environment>\nWorking directory: C:/work/project\nPlatform: windows\nGit repository: no\n</environment>"
    ));
    windows.selected_tools.push("bash".into());
    assert!(prompt(&windows).contains("Platform: windows\nShell: bash\nGit repository: no\n"));
    assert!(
        prompt(&options("/tmp", &[])).ends_with(
            "<environment>\nWorking directory: /tmp\nGit repository: no\n</environment>"
        )
    );
}

#[test]
fn run_modes() {
    let mut run = options("/tmp", &[]);
    assert!(prompt(&run).contains("<run>\nThis is a non-interactive run: no one can answer questions. Make reasonable assumptions, state them in your final answer, and finish the task.\n</run>"));
    run.run_mode = RunMode::Interactive;
    assert!(prompt(&run).contains("<run>\nThis is an interactive session. Ask the user only when you are blocked or before an action that is hard to reverse or visible to others.\n</run>"));
}

#[test]
fn optional_sections() {
    let text = prompt(&options("/tmp", &["read"]));
    assert!(text.starts_with(&format!("{PREAMBLE}\n\n<rules>\n")));
    assert!(!text.contains("<permissions>"));
    assert!(!text.contains("<docs>"));
    let mut with = options("/tmp", &["read"]);
    with.append_system_prompt = "Additional instructions.".into();
    with.context_files = vec![ContextFile {
        path: "/tmp/AGENTS.md".into(),
        content: "Project instructions.".into(),
    }];
    assert_eq!(
        names(&with),
        [
            "preamble",
            "rules",
            "run",
            "addendum",
            "project_context",
            "environment"
        ]
    );
    let text = prompt(&with);
    assert!(text.contains("<addendum>\nAdditional instructions.\n</addendum>"));
    assert!(text.contains(
        "<project_context>\nProject-specific instructions and guidelines:\n\n<project_instructions path=\"/tmp/AGENTS.md\">\nProject instructions.\n</project_instructions>\n</project_context>"
    ));
}

// Pi: "uses shell-specific guidance".
#[test]
fn shell_guidance() {
    let only_bash = prompt(&options("/tmp", &["bash"]));
    assert!(only_bash.contains("- Use bash for file operations like ls, rg, find"));
    assert!(!prompt(&options("/tmp", &["bash", "grep"])).contains("Use bash for file operations"));
    assert!(
        prompt(&options("/tmp", &["powershell"])).contains("Use PowerShell for file operations")
    );
    assert!(
        prompt(&options("/tmp", &["bash", "powershell"]))
            .contains("Use bash or PowerShell for file operations")
    );
}

// Pi: "appends promptGuidelines" and "deduplicates and trims
// promptGuidelines"; tool guidelines follow the selected tools' order.
#[test]
fn guidelines() {
    let mut with = options("/tmp", &["write", "read", "dynamic_tool"]);
    with.tool_guidelines = vec![
        ("read".into(), vec!["Read first.".into()]),
        ("write".into(), vec!["Write second.".into()]),
        ("unselected".into(), vec!["Never shown.".into()]),
    ];
    with.prompt_guidelines = vec![
        "Use dynamic_tool for summaries.".into(),
        "  Use dynamic_tool for summaries.  ".into(),
        "   ".into(),
        "Keep the final answer concise, factual, and neutral.".into(),
    ];
    let text = prompt(&with);
    assert!(text.contains(
        "- Keep the final answer concise, factual, and neutral.\n- Write second.\n- Read first.\n- Use dynamic_tool for summaries.\n</rules>"
    ));
    assert!(!text.contains("Never shown."));
    assert_eq!(text.matches("Keep the final answer").count(), 1);
}

// Pi: "leaves hidden tools out of the tool list and rules".
#[test]
fn hidden_tools() {
    let mut hidden = options("/tmp", &["read", "bash", "run"]);
    hidden.hidden_tools = vec!["read".into(), "bash".into()];
    hidden.environment.shell = "bash".into();
    hidden.tool_guidelines = vec![
        ("read".into(), vec!["Use read for files.".into()]),
        ("run".into(), vec!["Prefer run.".into()]),
    ];
    let text = prompt(&hidden);
    assert!(!text.contains("Use read for files."));
    assert!(!text.contains("Use bash for file operations"));
    assert!(!text.contains("Shell:"));
    assert!(text.contains("- Prefer run."));
    hidden.hidden_tools = vec!["read".into(), "bash".into(), "run".into()];
    assert!(!prompt(&hidden).contains("round trips"));
}

#[test]
fn invalid_section_names_are_refused() {
    for name in ["preamble", "Bad", "9x", ""] {
        let mut bad = options("/tmp", &[]);
        bad.sections = pairs(&[(name, "x")]);
        assert_eq!(
            build_system_prompt(&bad),
            Err(InvalidSectionName(name.to_owned()))
        );
    }
}

#[test]
fn diffs_sections_by_name() {
    let previous = Sections(vec![
        ("preamble".into(), Some("p".into())),
        ("rules".into(), Some("r".into())),
        ("gone".into(), Some("g".into())),
    ]);
    let current = pairs(&[("preamble", "p"), ("rules", "r2"), ("environment", "e")]);
    assert_eq!(
        diff_system_prompt_sections(&previous, &current),
        Some(Sections(vec![
            ("rules".into(), Some("r2".into())),
            ("environment".into(), Some("e".into())),
            ("gone".into(), None),
        ]))
    );
    let same = pairs(&[("preamble", "p"), ("rules", "r"), ("gone", "g")]);
    assert_eq!(diff_system_prompt_sections(&previous, &same), None);
}

#[test]
fn detects_a_git_ancestor_without_git() {
    let known = |paths: &'static [&'static str]| {
        move |path: &Path| paths.iter().any(|known| Path::new(known) == path)
    };
    let cwd = Path::new("/repo/crates/app");
    assert!(has_git_ancestor(cwd, known(&["/repo/crates/app/.git"])));
    // A worktree's or submodule's `.git` is a file; either counts.
    assert!(has_git_ancestor(cwd, known(&["/repo/.git"])));
    assert!(has_git_ancestor(cwd, known(&["/.git"])));
    assert!(!has_git_ancestor(
        cwd,
        known(&["/repo/crates/app/git", "/other/.git"])
    ));

    let dir = std::env::temp_dir().join(format!("bake-git-ancestor-{}", std::process::id()));
    let nested = dir.join("a").join("b");
    std::fs::create_dir_all(&nested).expect("create");
    std::fs::write(dir.join("a").join(".git"), "gitdir: elsewhere\n").expect("write");
    assert!(Environment::detect(&nested, 0).git_repository);
    std::fs::remove_dir_all(&dir).expect("remove");
}

#[test]
fn detects_this_platform_and_the_utc_date() {
    let environment = Environment::detect(Path::new("/"), 1_791_936_000_000);
    assert_eq!(environment.platform, std::env::consts::OS);
    assert_eq!(environment.started, "2026-10-14");
    assert_eq!(utc_date(1_791_935_999_999), "2026-10-13");
    assert_eq!(utc_date(0), "1970-01-01");
}
