//! The built-in tool sets and the tool selection, ported from Pi
//! `src/core/tools/index.ts`, the tool-list rules of
//! `src/core/settings-manager.ts` (`DEFAULT_TOOL_NAMES`, `getToolListError`,
//! `applyToolModifiers`, `defaultTools`), the tool options of
//! `src/core/sdk.ts`, the allowlist of `src/core/agent-session.ts`, and
//! `createToolNameMatcher` from `src/core/mcp-servers.ts` (v1.1.0; see the
//! crate's `NOTICE`).
//!
//! Pi's `bash` tool lives in [`crate::tools::bash`] and joins through
//! [`ToolsOptions::bash`] ([`create_bash_tool_definition`]); without it,
//! every set leaves `bash` out. Pi's `powershell` tool is not ported.

use std::path::Path;

use bake_agent::AgentTool;

use crate::tools::bash::{BashToolOptions, bash_tool_prompt, create_bash_tool};
use crate::tools::definition::ToolDefinition;
use crate::tools::edit::create_edit_tool_definition;
use crate::tools::find::create_find_tool_definition;
use crate::tools::grep::create_grep_tool_definition;
use crate::tools::ls::create_ls_tool_definition;
use crate::tools::read::{ReadToolOptions, create_read_tool_definition};
use crate::tools::write::create_write_tool_definition;

/// Pi's `allToolNames`, in Pi's order; `powershell` has no Bake port.
pub const ALL_TOOL_NAMES: [&str; 8] = [
    "read",
    "bash",
    "powershell",
    "edit",
    "write",
    "grep",
    "find",
    "ls",
];

/// Pi's `DEFAULT_TOOL_NAMES`: the tools a session starts with when
/// `defaultTools` does not change them.
pub const DEFAULT_TOOL_NAMES: [&str; 4] = ["read", "bash", "edit", "write"];

/// Pi's `ToolsOptions` for the ported tools.
#[derive(Debug, Clone, Default)]
pub struct ToolsOptions {
    /// Options of `read`.
    pub read: ReadToolOptions,
    /// The `bash` tool for this working directory, Pi's
    /// `createBashToolDefinition(cwd, options.bash)`. The bash port supplies
    /// it; `None` leaves `bash` out of every set.
    pub bash: Option<ToolDefinition>,
}

/// Pi's `createBashToolDefinition(cwd, options)`: the bash tool with its
/// prompt snippet, and its guideline unless the session environment is
/// hidden.
pub fn create_bash_tool_definition(cwd: &Path, options: BashToolOptions) -> ToolDefinition {
    let prompt = bash_tool_prompt(&options);
    ToolDefinition {
        tool: create_bash_tool(cwd, options),
        prompt_snippet: prompt.snippet,
        prompt_guidelines: (!prompt.guidelines.is_empty()).then_some(prompt.guidelines),
    }
}

/// Pi's `createToolDefinition`: `None` for `powershell`, for `bash` without
/// [`ToolsOptions::bash`], and for unknown names.
pub fn create_tool_definition(
    name: &str,
    cwd: &Path,
    options: &ToolsOptions,
) -> Option<ToolDefinition> {
    match name {
        "read" => Some(create_read_tool_definition(cwd, options.read.clone())),
        "bash" => options.bash.clone(),
        "edit" => Some(create_edit_tool_definition(cwd)),
        "write" => Some(create_write_tool_definition(cwd)),
        "grep" => Some(create_grep_tool_definition(cwd)),
        "find" => Some(create_find_tool_definition(cwd)),
        "ls" => Some(create_ls_tool_definition(cwd)),
        _ => None,
    }
}

/// Pi's `createTool`.
pub fn create_tool(name: &str, cwd: &Path, options: &ToolsOptions) -> Option<AgentTool> {
    create_tool_definition(name, cwd, options).map(|definition| definition.tool)
}

fn definitions(names: &[&str], cwd: &Path, options: &ToolsOptions) -> Vec<ToolDefinition> {
    names
        .iter()
        .filter_map(|name| create_tool_definition(name, cwd, options))
        .collect()
}

/// Pi's `createCodingToolDefinitions`: read, bash, edit, write.
pub fn create_coding_tool_definitions(cwd: &Path, options: &ToolsOptions) -> Vec<ToolDefinition> {
    definitions(&["read", "bash", "edit", "write"], cwd, options)
}

/// Pi's `createReadOnlyToolDefinitions`: read, grep, find, ls.
pub fn create_read_only_tool_definitions(
    cwd: &Path,
    options: &ToolsOptions,
) -> Vec<ToolDefinition> {
    definitions(&["read", "grep", "find", "ls"], cwd, options)
}

/// Pi's `createAllToolDefinitions`, in Pi's order.
pub fn create_all_tool_definitions(cwd: &Path, options: &ToolsOptions) -> Vec<ToolDefinition> {
    definitions(&ALL_TOOL_NAMES, cwd, options)
}

/// Pi's `createCodingTools`.
pub fn create_coding_tools(cwd: &Path, options: &ToolsOptions) -> Vec<AgentTool> {
    create_coding_tool_definitions(cwd, options)
        .into_iter()
        .map(|definition| definition.tool)
        .collect()
}

/// Pi's `createReadOnlyTools`.
pub fn create_read_only_tools(cwd: &Path, options: &ToolsOptions) -> Vec<AgentTool> {
    create_read_only_tool_definitions(cwd, options)
        .into_iter()
        .map(|definition| definition.tool)
        .collect()
}

/// Pi's `createAllTools`.
pub fn create_all_tools(cwd: &Path, options: &ToolsOptions) -> Vec<AgentTool> {
    create_all_tool_definitions(cwd, options)
        .into_iter()
        .map(|definition| definition.tool)
        .collect()
}

/// Pi's `isToolModifier`: a `+name` or `-name` entry.
pub fn is_tool_modifier(entry: &str) -> bool {
    entry.starts_with('+') || entry.starts_with('-')
}

/// Pi's `getToolListError`: a list is plain names and patterns, or only
/// `+name` and `-name` entries with exact names.
pub fn get_tool_list_error(entries: &[String]) -> Option<String> {
    let modifiers: Vec<&String> = entries
        .iter()
        .filter(|entry| is_tool_modifier(entry))
        .collect();
    if modifiers.is_empty() {
        return None;
    }
    if modifiers.len() < entries.len() {
        return Some("tool names cannot be mixed with +name or -name entries".into());
    }
    modifiers
        .iter()
        .find(|entry| entry.contains('*'))
        .map(|entry| {
            format!("+name and -name entries take exact tool names, not patterns: {entry}")
        })
}

/// Pi's `applyToolModifiers`: `+name` adds and `-name` removes, in order.
pub fn apply_tool_modifiers(base: &[String], entries: &[String]) -> Vec<String> {
    let mut tools = base.to_vec();
    for entry in entries {
        if !is_tool_modifier(entry) {
            continue;
        }
        let name = &entry[1..];
        let index = tools.iter().position(|tool| tool == name);
        if entry.starts_with('+') && index.is_none() && !name.is_empty() {
            tools.push(name.to_owned());
        } else if let (true, Some(index)) = (entry.starts_with('-'), index) {
            tools.remove(index);
        }
    }
    tools
}

fn defaults() -> Vec<String> {
    DEFAULT_TOOL_NAMES.map(str::to_owned).to_vec()
}

/// Pi's `resolveDefaultTools`: plain names replace the defaults, then the
/// modifiers apply.
pub fn resolve_default_tools(entries: &[String]) -> Vec<String> {
    let plain: Vec<String> = entries
        .iter()
        .filter(|entry| !is_tool_modifier(entry))
        .cloned()
        .collect();
    let base = if !plain.is_empty() || entries.is_empty() {
        plain
    } else {
        defaults()
    };
    apply_tool_modifiers(&base, entries)
}

/// Pi's `createToolNameMatcher`: exact names, and patterns where `*`
/// matches any run of characters other than line terminators.
pub fn tool_name_matcher(entries: &[String]) -> impl Fn(&str) -> bool + Send + Sync + 'static {
    let names: Vec<String> = entries
        .iter()
        .filter(|entry| !entry.contains('*'))
        .cloned()
        .collect();
    let patterns: Vec<Vec<String>> = entries
        .iter()
        .filter(|entry| entry.contains('*'))
        .map(|entry| entry.split('*').map(str::to_owned).collect())
        .collect();
    move |name: &str| {
        names.iter().any(|known| known == name)
            || patterns.iter().any(|parts| wildcard_match(parts, name))
    }
}

fn wildcard_match(parts: &[String], name: &str) -> bool {
    let Some((first, rest)) = parts.split_first() else {
        return name.is_empty();
    };
    let Some(mut remaining) = name.strip_prefix(first.as_str()) else {
        return false;
    };
    let Some((last, middle)) = rest.split_last() else {
        return remaining.is_empty();
    };
    let is_terminator = |c: char| matches!(c, '\n' | '\r' | '\u{2028}' | '\u{2029}');
    for part in middle {
        let Some(index) = remaining.find(part.as_str()) else {
            return false;
        };
        if remaining[..index].contains(is_terminator) {
            return false;
        }
        remaining = &remaining[index + part.len()..];
    }
    remaining.len() >= last.len()
        && remaining.ends_with(last.as_str())
        && !remaining[..remaining.len() - last.len()].contains(is_terminator)
}

/// Pi's `noTools` option.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NoTools {
    /// `--no-tools`: no tool at all.
    All,
    /// `--no-builtin-tools`: built-in tools registered but inactive.
    Builtin,
}

/// Pi's tool options of `createAgentSession`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ToolSelectionOptions {
    /// `--tools`: an allowlist of names and patterns, or `+name`/`-name`
    /// changes to the default selection.
    pub tools: Option<Vec<String>>,
    /// `--exclude-tools`: names and patterns to leave out.
    pub exclude_tools: Option<Vec<String>>,
    /// `--no-tools` or `--no-builtin-tools`.
    pub no_tools: Option<NoTools>,
}

/// The tools a session registers and starts with.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolSelection {
    /// The registered tools, in `available`'s order.
    pub registered: Vec<String>,
    /// The active tools, in Pi's order.
    pub active: Vec<String>,
}

/// Pi's selection of the initial tools from the options, the
/// `defaultTools` setting (already resolved), and the `available` tool
/// names, with no extension tools.
pub fn select_tools(
    options: &ToolSelectionOptions,
    default_tools: Option<&[String]>,
    available: &[String],
) -> Result<ToolSelection, String> {
    if let Some(error) = options.tools.as_deref().and_then(get_tool_list_error) {
        return Err(format!("Invalid tools option: {error}"));
    }
    let default_names: Vec<String> = if options.no_tools.is_some() {
        Vec::new()
    } else {
        default_tools.map_or_else(defaults, <[String]>::to_vec)
    };
    let modifiers = options
        .tools
        .as_ref()
        .filter(|tools| tools.iter().any(|entry| is_tool_modifier(entry)));
    let selected: Option<Vec<String>> = match modifiers {
        Some(modifiers) => Some(apply_tool_modifiers(&default_names, modifiers)),
        None => options.tools.clone(),
    };
    let allowed: Option<Vec<String>> = match modifiers {
        Some(_) if options.no_tools == Some(NoTools::All) => selected.clone(),
        Some(_) => None,
        None => options
            .tools
            .clone()
            .or_else(|| (options.no_tools == Some(NoTools::All)).then(Vec::new)),
    };
    let excluded = options.exclude_tools.as_deref().map(tool_name_matcher);
    let allowed_matcher = allowed.as_deref().map(tool_name_matcher);
    let is_allowed = |name: &str| {
        !excluded.as_ref().is_some_and(|excluded| excluded(name))
            && allowed_matcher.as_ref().is_none_or(|allowed| allowed(name))
    };
    let registered: Vec<String> = available
        .iter()
        .filter(|name| is_allowed(name))
        .cloned()
        .collect();
    let mut active: Vec<String> = selected
        .unwrap_or(default_names)
        .into_iter()
        .filter(|name| !excluded.as_ref().is_some_and(|excluded| excluded(name)))
        .filter(|name| is_allowed(name))
        .collect();
    if let Some(allowed) = &allowed_matcher {
        // Naming or matching a tool activates it.
        active.extend(registered.iter().filter(|name| allowed(name)).cloned());
    }
    let mut unique: Vec<String> = Vec::new();
    for name in active {
        if registered.contains(&name) && !unique.contains(&name) {
            unique.push(name);
        }
    }
    Ok(ToolSelection {
        registered,
        active: unique,
    })
}
