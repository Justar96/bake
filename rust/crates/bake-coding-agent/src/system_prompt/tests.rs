//! Ports Pi `test/system-prompt.test.ts` (v1.1.0), except its skills and
//! forced-prompt cases (not ported), and checks the prompt against
//! fixtures recorded from Pi's `buildSystemPrompt`.

use super::*;

fn docs() -> DocsPaths {
    DocsPaths {
        readme: "/opt/pi/README.md".into(),
        docs: "/opt/pi/docs".into(),
        examples: "/opt/pi/examples".into(),
    }
}

fn options(cwd: &str, tools: &[&str]) -> SystemPromptOptions {
    let mut options = SystemPromptOptions::new(cwd, docs());
    options.selected_tools = tools.iter().map(|tool| (*tool).to_owned()).collect();
    options
}

fn prompt(options: &SystemPromptOptions) -> String {
    build_system_prompt(options).unwrap_or_else(|error| error.to_string())
}

fn pairs(entries: &[(&str, &str)]) -> Vec<(String, String)> {
    entries
        .iter()
        .map(|(key, value)| ((*key).to_owned(), (*value).to_owned()))
        .collect()
}

/// Pi's four default tools with their prompt contributions
/// (`tools/{read,bash,edit,write}.ts`).
fn coding_options() -> SystemPromptOptions {
    let mut options = options("C:\\work\\project", &["read", "bash", "edit", "write"]);
    options.tool_snippets = pairs(&[
        ("read", "Read file contents"),
        ("bash", "Execute bash commands (ls, grep, find, etc.)"),
        (
            "edit",
            "Make precise file edits with exact text replacement, including multiple disjoint edits in one call",
        ),
        ("write", "Create or overwrite files"),
    ]);
    options.tool_guidelines = vec![
        ("read".into(), vec!["Use read to examine files instead of cat or sed.".into()]),
        (
            "bash".into(),
            vec!["You can inspect PI_* environment variables for current model and session details.".into()],
        ),
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
    options.context_files = vec![
        ContextFile {
            path: "/home/u/.bake/AGENTS.md".into(),
            content: "Global rules.".into(),
        },
        ContextFile {
            path: "/work/project/AGENTS.md".into(),
            content: "# Project\n\nUse tabs.".into(),
        },
    ];
    options
}

#[test]
fn matches_pi_fixtures() {
    // Recorded from Pi v1.1.0 with `PI_PACKAGE_DIR=/opt/pi`.
    assert_eq!(
        prompt(&coding_options()),
        include_str!("../../tests/fixtures/system_prompt/coding.txt")
    );
    assert_eq!(
        prompt(&options("/tmp/x", &[])),
        include_str!("../../tests/fixtures/system_prompt/no-tools.txt")
    );
    let mut grep = options("/tmp/x", &["bash", "grep", "ls"]);
    grep.tool_snippets = pairs(&[("bash", "Run"), ("grep", "Search")]);
    grep.prompt_guidelines = vec![
        "  Extra rule  ".into(),
        "Be concise in your responses".into(),
    ];
    assert_eq!(
        prompt(&grep),
        include_str!("../../tests/fixtures/system_prompt/grep-ls.txt")
    );
    let mut custom = SystemPromptOptions::new("/tmp/x", docs());
    custom.custom_prompt = Some("Custom preamble.".into());
    custom.append_system_prompt = "Appended.".into();
    custom.sections = pairs(&[("notes", "N"), ("cwd", "override")]);
    custom.context_files = vec![ContextFile {
        path: "a".into(),
        content: "b".into(),
    }];
    assert_eq!(
        prompt(&custom),
        include_str!("../../tests/fixtures/system_prompt/custom.txt")
    );
}

#[test]
fn sections_match_the_pi_fixture() {
    let sections = build_system_prompt_sections(&coding_options()).unwrap_or_default();
    let recorded: serde_json::Value = serde_json::from_str(include_str!(
        "../../tests/fixtures/system_prompt/coding.sections.json"
    ))
    .unwrap_or_default();
    let recorded: Vec<(String, String)> = recorded
        .as_object()
        .map(|object| {
            object
                .iter()
                .map(|(name, text)| (name.clone(), text.as_str().unwrap_or_default().to_owned()))
                .collect()
        })
        .unwrap_or_default();
    assert_eq!(sections, recorded);
}

// Pi: "shows (none) for empty tools list" and "shows file paths guideline
// even with no tools".
#[test]
fn empty_tools() {
    let text = prompt(&options("/tmp", &[]));
    assert!(text.contains("<tools>\n(none)\n"));
    assert!(text.contains("Show file paths clearly"));
}

// Pi: "keeps the default and custom prompt prefixes exact" and "maps
// appended instructions and project context to stable sections".
#[test]
fn prompt_structure() {
    assert!(
        prompt(&options("/tmp", &[]))
            .starts_with("You are an expert coding assistant operating inside pi")
    );
    let mut custom = options("/tmp", &[]);
    custom.custom_prompt = Some("You are Exact.".into());
    assert!(prompt(&custom).starts_with("You are Exact.\n\n<cwd>"));
    custom.append_system_prompt = "Additional instructions.".into();
    custom.context_files = vec![ContextFile {
        path: "/tmp/AGENTS.md".into(),
        content: "Project instructions.".into(),
    }];
    let text = prompt(&custom);
    assert!(text.contains("<addendum>\nAdditional instructions.\n</addendum>"));
    assert!(text.contains(
        "<project_context>\nProject-specific instructions and guidelines:\n\n<project_instructions path=\"/tmp/AGENTS.md\">"
    ));
    assert!(text.contains("<cwd>\n/tmp\n</cwd>"));
}

// Pi: "includes all default tools when snippets are provided", "uses
// shell-specific guidance", and "instructs models to resolve pi docs and
// examples under absolute base paths".
#[test]
fn default_tools() {
    let mut defaults = SystemPromptOptions::new("/tmp", docs());
    defaults.tool_snippets = pairs(&[
        ("read", "Read file contents"),
        ("bash", "Execute bash commands"),
        ("edit", "Make surgical edits"),
        ("write", "Create or overwrite files"),
    ]);
    let text = prompt(&defaults);
    for tool in ["- read:", "- bash:", "- edit:", "- write:"] {
        assert!(text.contains(tool), "{tool}");
    }
    assert!(text.contains("- When reading pi docs or examples, resolve docs/... under Additional docs and examples/... under Examples, not the current working directory"));
    assert!(text.contains(
        "environment variables (docs/environment-variables.md), MCP servers (docs/mcp.md)"
    ));
    assert!(
        prompt(&options("/tmp", &["powershell"])).contains("Use PowerShell for file operations")
    );
    assert!(
        prompt(&options("/tmp", &["bash", "powershell"]))
            .contains("Use bash or PowerShell for file operations")
    );
}

// Pi: "includes custom tools ... when promptSnippet is provided", "omits
// custom tools ... when promptSnippet is not provided", "appends
// promptGuidelines", and "deduplicates and trims promptGuidelines".
#[test]
fn custom_snippets_and_guidelines() {
    let mut with = options("/tmp", &["read", "dynamic_tool"]);
    with.tool_snippets = pairs(&[("dynamic_tool", "Run dynamic test behavior")]);
    assert!(prompt(&with).contains("- dynamic_tool: Run dynamic test behavior"));
    let mut without = options("/tmp", &["read", "dynamic_tool"]);
    assert!(!prompt(&without).contains("dynamic_tool"));
    without.prompt_guidelines = vec!["Use dynamic_tool for project summaries.".into()];
    assert!(prompt(&without).contains("- Use dynamic_tool for project summaries."));
    without.prompt_guidelines = vec![
        "Use dynamic_tool for summaries.".into(),
        "  Use dynamic_tool for summaries.  ".into(),
        "   ".into(),
    ];
    assert_eq!(
        prompt(&without)
            .matches("- Use dynamic_tool for summaries.")
            .count(),
        1
    );
}

// Pi: "leaves hidden tools out of the tool list and rules".
#[test]
fn hidden_tools() {
    let mut hidden = options("/tmp", &["read", "bash", "run"]);
    hidden.hidden_tools = vec!["read".into(), "bash".into()];
    hidden.tool_snippets = pairs(&[
        ("read", "Read files"),
        ("bash", "Run commands"),
        ("run", "Run a task"),
    ]);
    hidden.tool_guidelines = vec![
        ("read".into(), vec!["Use read for files.".into()]),
        ("run".into(), vec!["Prefer run.".into()]),
    ];
    let text = prompt(&hidden);
    assert!(text.contains("<tools>\n- run: Run a task\n"));
    assert!(!text.contains("- read: "));
    assert!(!text.contains("Use read for files."));
    assert!(!text.contains("Use bash for file operations"));
    assert!(text.contains("- Prefer run."));
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
        ("tools".into(), Some("t".into())),
        ("gone".into(), Some("g".into())),
    ]);
    let current = pairs(&[("preamble", "p"), ("tools", "t2"), ("cwd", "c")]);
    assert_eq!(
        diff_system_prompt_sections(&previous, &current),
        Some(Sections(vec![
            ("tools".into(), Some("t2".into())),
            ("cwd".into(), Some("c".into())),
            ("gone".into(), None),
        ]))
    );
    let same = pairs(&[("preamble", "p"), ("tools", "t"), ("gone", "g")]);
    assert_eq!(diff_system_prompt_sections(&previous, &same), None);
    assert_eq!(
        normalize_prompt_snippet(Some(" a\r\n  b\tc ")),
        Some("a b c".to_owned())
    );
    assert_eq!(normalize_prompt_snippet(Some(" \n ")), None);
}
