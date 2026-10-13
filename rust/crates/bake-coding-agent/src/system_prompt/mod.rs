//! The structured system prompt and the context files it includes.
//!
//! The prompt's text is Bake's own (D33,
//! `docs/roadmap/rust-0.4/system-prompt.md`); its mechanism is ported from
//! Pi `packages/coding-agent/src/core/system-prompt.ts` (v1.1.0), and from
//! `resource-loader.ts` the loading of `AGENTS.md` and `CLAUDE.md` context
//! files and of the `--system-prompt` and `--append-system-prompt` inputs
//! ([`context`]). The tools contribute their guidelines, which follow Pi.
//!
//! The prompt is a set of named sections: `preamble` is untagged, and every
//! other section is wrapped in a tag of its name so that a later system
//! message can replace it. A session records the sections in a system
//! message and later sends only the sections that changed
//! ([`diff_system_prompt_sections`]), so the sections whose facts vary come
//! last.
//!
//! Not ported: skills (scope 10; the `skills` section is never present),
//! and the forced prompt of an extension's `before_agent_start` handler.

pub mod context;

use std::path::Path;

use bake_ai::utils::text::get_system_message_text;
use bake_ai::{Sections, SystemContent, SystemMessage};

use crate::permissions::context::PERMISSIONS_SECTION;
use crate::session::time::format_iso;
use crate::tools::bash::get_shell_config;

pub use context::{ContextFile, load_project_context_files, resolve_prompt_input};

/// How the session runs, which the `run` section states.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum RunMode {
    /// `bake-rs -p`: no one can answer a question.
    #[default]
    Print,
    /// A user at the terminal can answer.
    Interactive,
}

/// The facts the `environment` section states; an empty fact is left out.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Environment {
    /// The operating system family, as `std::env::consts::OS` names it.
    pub platform: String,
    /// The file name of the shell the `bash` tool runs, stated while `bash`
    /// is selected.
    pub shell: String,
    /// Whether the working directory is inside a git repository.
    pub git_repository: bool,
    /// The UTC date the session started, `YYYY-MM-DD`.
    pub started: String,
}

impl Environment {
    /// The facts for a session in `cwd` that started at `started_ms`: this
    /// platform, the shell the `bash` tool would resolve, and whether `cwd`
    /// or an ancestor holds `.git` (a directory, or the file a worktree or
    /// submodule has). Git itself is not run, and `GIT_DIR` is not read.
    pub fn detect(cwd: &Path, started_ms: i64) -> Self {
        let shell = get_shell_config(None)
            .ok()
            .and_then(|config| {
                config
                    .shell
                    .file_stem()
                    .map(|stem| stem.to_string_lossy().into_owned())
            })
            .unwrap_or_default();
        Self {
            platform: std::env::consts::OS.to_owned(),
            shell,
            git_repository: has_git_ancestor(cwd, |path| path.symlink_metadata().is_ok()),
            started: utc_date(started_ms),
        }
    }
}

/// Whether `cwd` or one of its ancestors holds a `.git` entry.
pub fn has_git_ancestor(cwd: &Path, exists: impl Fn(&Path) -> bool) -> bool {
    cwd.ancestors()
        .any(|directory| exists(&directory.join(".git")))
}

/// The UTC calendar date of `ms`, `YYYY-MM-DD`.
pub fn utc_date(ms: i64) -> String {
    format_iso(ms).chars().take_while(|c| *c != 'T').collect()
}

/// The inputs of [`build_system_prompt_sections`]: Pi's
/// `BuildSystemPromptOptions` without skills or a forced prompt, with
/// Bake's permission, documentation, run, and environment facts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SystemPromptOptions {
    /// Replaces the default preamble and its rule and docs sections.
    pub custom_prompt: Option<String>,
    /// The selected tools; Pi's default is `read`, `bash`, `edit`, `write`.
    pub selected_tools: Vec<String>,
    /// Selected tools whose declarations requests leave out.
    pub hidden_tools: Vec<String>,
    /// Guideline bullets by tool name.
    pub tool_guidelines: Vec<(String, Vec<String>)>,
    /// Further guideline bullets.
    pub prompt_guidelines: Vec<String>,
    /// The permission statement; empty for none.
    pub permissions: String,
    /// The command that lists Bake's documentation topics, such as
    /// `/usr/local/bin/bake-rs docs`, already shell-quoted; empty for none.
    pub docs_command: String,
    /// How the session runs.
    pub run_mode: RunMode,
    /// Text appended before the project context.
    pub append_system_prompt: String,
    /// More sections by tag name.
    pub sections: Vec<(String, String)>,
    /// The working directory.
    pub cwd: String,
    /// The environment facts besides the working directory.
    pub environment: Environment,
    /// Loaded context files.
    pub context_files: Vec<ContextFile>,
}

impl SystemPromptOptions {
    /// Options for `cwd` with Pi's default tool selection, a print run, and
    /// nothing else.
    pub fn new(cwd: impl Into<String>) -> Self {
        Self {
            custom_prompt: None,
            selected_tools: ["read", "bash", "edit", "write"]
                .map(str::to_owned)
                .to_vec(),
            hidden_tools: Vec::new(),
            tool_guidelines: Vec::new(),
            prompt_guidelines: Vec::new(),
            permissions: String::new(),
            docs_command: String::new(),
            run_mode: RunMode::Print,
            append_system_prompt: String::new(),
            sections: Vec::new(),
            cwd: cwd.into(),
            environment: Environment::default(),
            context_files: Vec::new(),
        }
    }
}

/// An invalid custom section name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InvalidSectionName(pub String);

impl std::fmt::Display for InvalidSectionName {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "Invalid system prompt section name: {}", self.0)
    }
}

impl std::error::Error for InvalidSectionName {}

fn is_section_name(name: &str) -> bool {
    let bytes = name.as_bytes();
    bytes.first().is_some_and(u8::is_ascii_lowercase)
        && bytes
            .iter()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || *b == b'_' || *b == b'-')
}

fn render_project_context(files: &[ContextFile]) -> String {
    let mut parts = vec!["Project-specific instructions and guidelines:".to_owned()];
    parts.extend(files.iter().map(|file| {
        format!(
            "<project_instructions path=\"{}\">\n{}\n</project_instructions>",
            file.path, file.content
        )
    }));
    parts.join("\n\n")
}

/// The preamble of Bake's default prompt.
pub const PREAMBLE: &str = "You are Bake, a coding agent that works in the user's workspace by reading code, running commands, and editing files.";

/// Bake's first rules, from Bake 0.3's headless prompt
/// (`packages/bundle/headless/cordis.patch.yml`); the precedence sentence
/// follows Codex CLI's base instructions.
const LEADING_RULES: [&str; 2] = [
    "Follow the conventions of the surrounding code and the project's instructions. The user's requests take precedence over project instructions.",
    "Commit or push only when the task requests it, and do not discard changes you did not make.",
];

/// Bake 0.3's round-trip paragraph, for a session with tools, without the
/// sentence `edit`'s own guideline states. Calls in one response do run
/// in order: the session makes the tools that change files or run commands
/// sequential (`agent_session::ORDERED_TOOLS`).
const ROUND_TRIP_RULE: &str = "Work in as few model round trips as you can. Put independent tool calls in one response: read the files you need together, and send an edit with the command that checks it, since calls in one response run in order. Do not re-read a file to confirm an edit that succeeded.";

/// Bake 0.3's last rules, before the tool-contributed guidelines.
const TRAILING_RULES: [&str; 2] = [
    "Report results and verification accurately, including failures and skipped checks.",
    "Keep the final answer concise, factual, and neutral.",
];

fn build_rules(
    selected: &[String],
    tool_guidelines: &[(String, Vec<String>)],
    prompt_guidelines: &[String],
) -> String {
    let mut rules: Vec<String> = Vec::new();
    let mut add = |rule: &str| {
        let normalized = rule.trim();
        if !normalized.is_empty() && !rules.iter().any(|known| known == normalized) {
            rules.push(normalized.to_owned());
        }
    };
    LEADING_RULES.iter().for_each(|rule| add(rule));
    if !selected.is_empty() {
        add(ROUND_TRIP_RULE);
    }
    TRAILING_RULES.iter().for_each(|rule| add(rule));
    let has = |name: &str| selected.iter().any(|tool| tool == name);
    let (bash, powershell) = (has("bash"), has("powershell"));
    if (bash || powershell) && !has("grep") && !has("find") && !has("ls") {
        if bash && powershell {
            add(
                "Use bash or PowerShell for file operations like listing, searching, and finding files",
            );
        } else if powershell {
            add("Use PowerShell for file operations like listing, searching, and finding files");
        } else {
            add("Use bash for file operations like ls, rg, find");
        }
    }
    for name in selected {
        for (tool, guidelines) in tool_guidelines {
            if tool == name {
                guidelines.iter().for_each(|rule| add(rule));
            }
        }
    }
    prompt_guidelines.iter().for_each(|rule| add(rule));
    rules
        .iter()
        .map(|rule| format!("- {rule}"))
        .collect::<Vec<_>>()
        .join("\n")
}

fn run_text(mode: RunMode) -> &'static str {
    match mode {
        RunMode::Print => {
            "This is a non-interactive run: no one can answer questions. Make reasonable assumptions, state them in your final answer, and finish the task."
        }
        RunMode::Interactive => {
            "This is an interactive session. Ask the user only when you are blocked or before an action that is hard to reverse or visible to others."
        }
    }
}

fn environment_text(cwd: &str, environment: &Environment, bash: bool) -> String {
    let mut lines = vec![format!("Working directory: {}", cwd.replace('\\', "/"))];
    if !environment.platform.is_empty() {
        lines.push(format!("Platform: {}", environment.platform));
    }
    if bash && !environment.shell.is_empty() {
        lines.push(format!("Shell: {}", environment.shell));
    }
    lines.push(format!(
        "Git repository: {}",
        if environment.git_repository {
            "yes"
        } else {
            "no"
        }
    ));
    if !environment.started.is_empty() {
        lines.push(format!("Session started: {} (UTC)", environment.started));
    }
    lines.join("\n")
}

/// Sets a JavaScript object member: an existing key keeps its position.
fn set_section(sections: &mut Vec<(String, String)>, name: &str, content: String) {
    match sections.iter_mut().find(|(known, _)| known == name) {
        Some(entry) => entry.1 = content,
        None => sections.push((name.to_owned(), content)),
    }
}

/// Pi's `buildSystemPromptSections` with Bake's sections: `preamble`,
/// `rules`, `docs`, `run`, `addendum`, `project_context`, `permissions`,
/// `environment`, then [`SystemPromptOptions::sections`], from the most
/// stable to the most likely to differ between sessions. A custom prompt
/// replaces `preamble`, `rules`, and `docs`. Every section but `preamble`
/// is wrapped in its tag.
pub fn build_system_prompt_sections(
    options: &SystemPromptOptions,
) -> Result<Vec<(String, String)>, InvalidSectionName> {
    for (name, _) in &options.sections {
        if !is_section_name(name) || name == "preamble" {
            return Err(InvalidSectionName(name.clone()));
        }
    }
    let declared: Vec<String> = options
        .selected_tools
        .iter()
        .filter(|name| !options.hidden_tools.contains(name))
        .cloned()
        .collect();
    let mut sections: Vec<(String, String)> = Vec::new();
    match options
        .custom_prompt
        .as_deref()
        .filter(|prompt| !prompt.is_empty())
    {
        Some(custom) => set_section(&mut sections, "preamble", custom.to_owned()),
        None => {
            set_section(&mut sections, "preamble", PREAMBLE.to_owned());
            set_section(
                &mut sections,
                "rules",
                build_rules(
                    &declared,
                    &options.tool_guidelines,
                    &options.prompt_guidelines,
                ),
            );
            if !options.docs_command.is_empty() {
                set_section(
                    &mut sections,
                    "docs",
                    format!(
                        "For questions about Bake itself, such as its settings, models, permissions, or sessions, run `{docs}` to list the topics and `{docs} <topic>` to read one.",
                        docs = options.docs_command
                    ),
                );
            }
        }
    }
    set_section(&mut sections, "run", run_text(options.run_mode).to_owned());
    if !options.append_system_prompt.is_empty() {
        set_section(
            &mut sections,
            "addendum",
            options.append_system_prompt.clone(),
        );
    }
    if !options.context_files.is_empty() {
        set_section(
            &mut sections,
            "project_context",
            render_project_context(&options.context_files),
        );
    }
    if !options.permissions.is_empty() {
        set_section(
            &mut sections,
            PERMISSIONS_SECTION,
            options.permissions.clone(),
        );
    }
    set_section(
        &mut sections,
        "environment",
        environment_text(
            &options.cwd,
            &options.environment,
            declared.iter().any(|tool| tool == "bash"),
        ),
    );
    for (name, content) in &options.sections {
        if !content.is_empty() {
            set_section(&mut sections, name, content.clone());
        }
    }
    Ok(sections
        .into_iter()
        .map(|(name, content)| {
            if name == "preamble" {
                (name, content)
            } else {
                let wrapped = format!("<{name}>\n{content}\n</{name}>");
                (name, wrapped)
            }
        })
        .collect())
}

/// The system message that carries `sections` in full.
pub fn sections_message(sections: &[(String, String)], timestamp: i64) -> SystemMessage {
    SystemMessage {
        content: SystemContent::Text(String::new()),
        sections: Some(Sections(
            sections
                .iter()
                .map(|(name, text)| (name.clone(), Some(text.clone())))
                .collect(),
        )),
        tools_added: None,
        tools_removed: None,
        timestamp,
    }
}

/// Pi's `buildSystemPrompt`: the prompt as the transcript's system message
/// replays it.
pub fn build_system_prompt(options: &SystemPromptOptions) -> Result<String, InvalidSectionName> {
    let sections = build_system_prompt_sections(options)?;
    Ok(get_system_message_text(&sections_message(&sections, 0)))
}

/// Pi's `diffSystemPromptSections`: the patch from the sections the model
/// has to the desired ones, `None` values removing sections; `None` when
/// nothing changed.
pub fn diff_system_prompt_sections(
    previous: &Sections,
    current: &[(String, String)],
) -> Option<Sections> {
    let mut patch: Vec<(String, Option<String>)> = Vec::new();
    let previous_text = |name: &str| {
        previous
            .0
            .iter()
            .find(|(known, _)| known == name)
            .and_then(|(_, text)| text.as_deref())
    };
    for (name, text) in current {
        if previous_text(name) != Some(text.as_str()) {
            patch.push((name.clone(), Some(text.clone())));
        }
    }
    for (name, _) in &previous.0 {
        if !current.iter().any(|(known, _)| known == name)
            && !patch.iter().any(|(known, _)| known == name)
        {
            patch.push((name.clone(), None));
        }
    }
    (!patch.is_empty()).then_some(Sections(patch))
}

/// Pi's `_normalizePromptGuidelines`: trimmed, non-empty, unique, in order.
pub fn normalize_prompt_guidelines(guidelines: &[String]) -> Vec<String> {
    let mut unique: Vec<String> = Vec::new();
    for guideline in guidelines {
        let normalized = guideline.trim();
        if !normalized.is_empty() && !unique.iter().any(|known| known == normalized) {
            unique.push(normalized.to_owned());
        }
    }
    unique
}

#[cfg(test)]
mod tests;
