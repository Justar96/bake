//! The structured system prompt and the context files it includes.
//!
//! Ported from Pi `packages/coding-agent/src/core/system-prompt.ts`
//! (v1.1.0), and from `resource-loader.ts` the loading of `AGENTS.md` and
//! `CLAUDE.md` context files and of the `--system-prompt` and
//! `--append-system-prompt` inputs ([`context`]). The text is Pi's byte for
//! byte, including the paragraph that points at Pi's documentation; the
//! paths it names are [`DocsPaths`], which the caller chooses. Pi v1.1.0's
//! prompt carries no date.
//!
//! The prompt is a set of named sections: `preamble` is untagged, and every
//! other section is wrapped in a tag of its name so that a later system
//! message can replace it. A session records the sections in a system
//! message and later sends only the sections that changed
//! ([`diff_system_prompt_sections`]).
//!
//! Not ported: skills (scope 10; the `skills` section is never present),
//! and the forced prompt of an extension's `before_agent_start` handler.

pub mod context;

use bake_ai::utils::text::get_system_message_text;
use bake_ai::{Sections, SystemContent, SystemMessage};

pub use context::{ContextFile, load_project_context_files, resolve_prompt_input};

/// The documentation paths Pi's prompt names: its package's `README.md`,
/// `docs`, and `examples`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DocsPaths {
    /// Pi's `getReadmePath()`.
    pub readme: String,
    /// Pi's `getDocsPath()`.
    pub docs: String,
    /// Pi's `getExamplesPath()`.
    pub examples: String,
}

impl DocsPaths {
    /// Paths under a package directory, as Pi resolves them from its own.
    pub fn under(package_dir: &std::path::Path) -> Self {
        let path = |name: &str| package_dir.join(name).to_string_lossy().into_owned();
        Self {
            readme: path("README.md"),
            docs: path("docs"),
            examples: path("examples"),
        }
    }
}

/// Pi's `BuildSystemPromptOptions` without skills or a forced prompt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SystemPromptOptions {
    /// Replaces the default preamble and its tool, rule, and docs sections.
    pub custom_prompt: Option<String>,
    /// Tools to list; Pi's default is `read`, `bash`, `edit`, `write`.
    pub selected_tools: Vec<String>,
    /// Selected tools whose declarations requests leave out.
    pub hidden_tools: Vec<String>,
    /// One-line tool snippets by tool name; tools without one are not
    /// listed.
    pub tool_snippets: Vec<(String, String)>,
    /// Guideline bullets by tool name.
    pub tool_guidelines: Vec<(String, Vec<String>)>,
    /// Further guideline bullets.
    pub prompt_guidelines: Vec<String>,
    /// Text appended before the project context.
    pub append_system_prompt: String,
    /// More sections by tag name.
    pub sections: Vec<(String, String)>,
    /// The working directory.
    pub cwd: String,
    /// Loaded context files.
    pub context_files: Vec<ContextFile>,
    /// The documentation paths.
    pub docs: DocsPaths,
}

impl SystemPromptOptions {
    /// Options for `cwd` with Pi's default tool selection and nothing else.
    pub fn new(cwd: impl Into<String>, docs: DocsPaths) -> Self {
        Self {
            custom_prompt: None,
            selected_tools: ["read", "bash", "edit", "write"]
                .map(str::to_owned)
                .to_vec(),
            hidden_tools: Vec::new(),
            tool_snippets: Vec::new(),
            tool_guidelines: Vec::new(),
            prompt_guidelines: Vec::new(),
            append_system_prompt: String::new(),
            sections: Vec::new(),
            cwd: cwd.into(),
            context_files: Vec::new(),
            docs,
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
    add("Be concise in your responses");
    add("Show file paths clearly when working with files");
    rules
        .iter()
        .map(|rule| format!("- {rule}"))
        .collect::<Vec<_>>()
        .join("\n")
}

/// Sets a JavaScript object member: an existing key keeps its position.
fn set_section(sections: &mut Vec<(String, String)>, name: &str, content: String) {
    match sections.iter_mut().find(|(known, _)| known == name) {
        Some(entry) => entry.1 = content,
        None => sections.push((name.to_owned(), content)),
    }
}

/// Pi's `buildSystemPromptSections`: the ordered sections, every one but
/// `preamble` wrapped in its tag.
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
    let snippet = |name: &str| {
        options
            .tool_snippets
            .iter()
            .rev()
            .find(|(tool, snippet)| tool == name && !snippet.is_empty())
            .map(|(_, snippet)| snippet.as_str())
    };
    let mut sections: Vec<(String, String)> = Vec::new();
    match options
        .custom_prompt
        .as_deref()
        .filter(|prompt| !prompt.is_empty())
    {
        Some(custom) => set_section(&mut sections, "preamble", custom.to_owned()),
        None => {
            set_section(
                &mut sections,
                "preamble",
                "You are an expert coding assistant operating inside pi, a coding agent harness. You help users by reading files, executing commands, editing code, and writing new files.".to_owned(),
            );
            let visible: Vec<String> = declared
                .iter()
                .filter_map(|name| Some(format!("- {name}: {}", snippet(name)?)))
                .collect();
            let tools = if visible.is_empty() {
                "(none)".to_owned()
            } else {
                visible.join("\n")
            };
            set_section(
                &mut sections,
                "tools",
                format!(
                    "{tools}\n\nIn addition to the tools above, you may have access to other custom tools depending on the project."
                ),
            );
            set_section(
                &mut sections,
                "rules",
                build_rules(
                    &declared,
                    &options.tool_guidelines,
                    &options.prompt_guidelines,
                ),
            );
            let DocsPaths {
                readme,
                docs,
                examples,
            } = &options.docs;
            set_section(
                &mut sections,
                "docs",
                format!(
                    "Pi documentation (read only when the user asks about pi itself, its SDK, extensions, themes, skills, or TUI):
- Main documentation: {readme}
- Additional docs: {docs}
- Examples: {examples} (extensions, custom tools, SDK)
- When reading pi docs or examples, resolve docs/... under Additional docs and examples/... under Examples, not the current working directory
- When asked about: extensions (docs/extensions.md, examples/extensions/), themes (docs/themes.md), skills (docs/skills.md), prompt templates (docs/prompt-templates.md), TUI components (docs/tui.md), keybindings (docs/keybindings.md), SDK integrations (docs/sdk.md), custom providers (docs/custom-provider.md), adding models (docs/models.md), pi packages (docs/packages.md), environment variables (docs/environment-variables.md), MCP servers (docs/mcp.md), codemode scripts and non-LLM models such as classifiers and image models (docs/codemode.md)
- When working on pi topics, read the docs and examples, and follow .md cross-references before implementing
- Always read pi .md files completely and follow links to related docs (e.g., tui.md for TUI API details)"
                ),
            );
        }
    }
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
    set_section(&mut sections, "cwd", options.cwd.replace('\\', "/"));
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

/// Pi's `_normalizePromptSnippet`: one line with collapsed whitespace, or
/// `None` when empty.
pub fn normalize_prompt_snippet(text: Option<&str>) -> Option<String> {
    let text = text?;
    let one_line = text.split_whitespace().collect::<Vec<_>>().join(" ");
    (!one_line.is_empty()).then_some(one_line)
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
