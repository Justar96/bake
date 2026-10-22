//! File-based prompt templates (`/name args`), ported from Pi
//! `packages/coding-agent/src/core/prompt-templates.ts` and
//! `DefaultResourceLoader.dedupePrompts` in `src/core/resource-loader.ts`
//! (v1.2.0, revision `06a466935536f8c9bcacb2d5efc0fc021be1bdff`, unchanged
//! since v1.1.0).
//!
//! # Where prompt templates come from
//!
//! With [`LoadPromptTemplatesOptions::include_defaults`] set,
//! [`load_prompt_templates`] reads Markdown (`.md`) files from:
//!
//! 1. `<cwd>/.bake/prompts` in the working directory (`project`);
//! 2. `<agent_dir>/prompts` in the Bake home (`user`);
//! 3. each explicit `--prompt-template <path>` file or directory (`path`).
//!
//! This is Pi's `resourcePrecedenceRank` order (`package-manager.ts`):
//! project templates before user templates, explicit paths last.
//! Directories are scanned non-recursively in sorted filename order, following
//! symlinks to files and skipping broken symlinks. In the two default
//! directories, Pi's `collectAutoPromptEntries` also skips names starting with
//! `.`, `node_modules`, and files matched by the directory's `.gitignore`,
//! `.ignore`, or `.fdignore`. The first template of each name wins, and a
//! later template with the same name from another file is recorded as a
//! collision diagnostic. An explicit path that does not exist is an error
//! diagnostic.

use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};

use serde_json::Value;

use crate::auth_storage::read_text_file;
use crate::session::paths::{lexical_normalize, normalize_path, resolve_path};
use crate::skills::{IgnoreRules, parse_frontmatter};
use crate::tools::js::{is_whitespace, trim};

/// The largest prompt template file read, in bytes.
pub const MAX_PROMPT_TEMPLATE_FILE_BYTES: u64 = 1024 * 1024;

/// A prompt template loaded from a Markdown file, Pi's `PromptTemplate`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PromptTemplate {
    /// Template name (the file stem without `.md`).
    pub name: String,
    /// Description from frontmatter or the first non-empty line of the body.
    pub description: String,
    /// Optional `argument-hint` from frontmatter.
    pub argument_hint: Option<String>,
    /// Template body without frontmatter.
    pub content: String,
    /// Where it was found: `user`, `project`, or `path`.
    pub source: String,
    /// Absolute path to the template file.
    pub file_path: String,
}

/// The two files of a template name collision.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PromptTemplateCollision {
    /// The shared template name.
    pub name: String,
    /// The template that was kept.
    pub winner_path: String,
    /// The template that was skipped.
    pub loser_path: String,
}

/// A diagnostic's kind, Pi's `ResourceDiagnostic.type` for prompt templates.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PromptTemplateDiagnosticKind {
    /// A problem reading or parsing a template file or path.
    Warning,
    /// An explicit `--prompt-template` path that does not exist.
    Error,
    /// A second template with an already-loaded template's name.
    Collision,
}

/// A problem found while loading prompt templates, Pi's `ResourceDiagnostic`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PromptTemplateDiagnostic {
    /// Its kind.
    pub kind: PromptTemplateDiagnosticKind,
    /// Diagnostic message.
    pub message: String,
    /// The file or path concerned.
    pub path: String,
    /// Collision metadata for [`PromptTemplateDiagnosticKind::Collision`].
    pub collision: Option<PromptTemplateCollision>,
}

impl PromptTemplateDiagnostic {
    fn warning(message: impl Into<String>, path: &Path) -> Self {
        Self {
            kind: PromptTemplateDiagnosticKind::Warning,
            message: message.into(),
            path: path.to_string_lossy().into_owned(),
            collision: None,
        }
    }
}

/// Options for [`load_prompt_templates`], Pi's `LoadPromptTemplatesOptions`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LoadPromptTemplatesOptions {
    /// Working directory for project-local templates and relative paths.
    pub cwd: String,
    /// Bake home directory for global templates (`<agent_dir>/prompts`).
    pub agent_dir: PathBuf,
    /// Explicit prompt template paths (`--prompt-template`), files or directories.
    pub prompt_paths: Vec<String>,
    /// Whether to include default prompt directories (`--no-prompt-templates` clears it).
    pub include_defaults: bool,
}

/// Loaded prompt templates and diagnostics, Pi's `LoadPromptTemplatesResult`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LoadPromptTemplatesResult {
    /// Loaded templates, deduplicated by name in load order.
    pub templates: Vec<PromptTemplate>,
    /// Warnings and collision diagnostics.
    pub diagnostics: Vec<PromptTemplateDiagnostic>,
}

/// Pi's `parseCommandArgs`: parse command arguments respecting single- and
/// double-quoted strings (bash-style).
pub fn parse_command_args(args_string: &str) -> Vec<String> {
    let mut args = Vec::new();
    let mut current = String::new();
    let mut in_quote: Option<char> = None;

    for ch in args_string.chars() {
        if let Some(quote) = in_quote {
            if ch == quote {
                in_quote = None;
            } else {
                current.push(ch);
            }
        } else if ch == '"' || ch == '\'' {
            in_quote = Some(ch);
        } else if is_whitespace(ch) {
            if !current.is_empty() {
                args.push(std::mem::take(&mut current));
            }
        } else {
            current.push(ch);
        }
    }

    if !current.is_empty() {
        args.push(current);
    }

    args
}

/// Pi's `substituteArgs`: substitute argument placeholders in template content.
///
/// Supports:
/// - `$1`, `$2`, ... for positional args
/// - `$@` and `$ARGUMENTS` for all args
/// - `${N:-default}` for positional arg `N` with default when missing/empty
/// - `${@:-default}` and `${ARGUMENTS:-default}` for all args with default when empty
/// - `${@:N}` for args from `N`th onwards (1-indexed; `0` treated as `1`)
/// - `${@:N:L}` for `L` args starting from `N`th
///
/// Replacement happens in a single pass over `content`: argument and default
/// values containing `$1`, `$@`, or `$ARGUMENTS` are never recursively
/// substituted.
pub fn substitute_args(content: &str, args: &[String]) -> String {
    let all_args = args.join(" ");
    let mut out = String::with_capacity(content.len());
    let mut index = 0;

    while index < content.len() {
        let rest = &content[index..];
        let Some(dollar_offset) = rest.find('$') else {
            out.push_str(rest);
            break;
        };
        out.push_str(&rest[..dollar_offset]);
        index += dollar_offset;
        let after_dollar = &content[index + 1..];

        if let Some((consumed, replacement)) = match_placeholder(after_dollar, args, &all_args) {
            out.push_str(&replacement);
            index += 1 + consumed;
        } else {
            out.push('$');
            index += 1;
        }
    }

    out
}

fn leading_digits(text: &str) -> &str {
    let len = text.bytes().take_while(u8::is_ascii_digit).count();
    &text[..len]
}

fn positional_arg<'a>(digits: &str, args: &'a [String]) -> Option<&'a str> {
    let number: usize = digits.parse().ok()?;
    let idx = number.checked_sub(1)?;
    args.get(idx).map(String::as_str)
}

fn match_placeholder(
    after_dollar: &str,
    args: &[String],
    all_args: &str,
) -> Option<(usize, String)> {
    if let Some(braced) = after_dollar.strip_prefix('{') {
        // 1. `\$\{(\d+|ARGUMENTS|@):-([^}]*)\}`
        let default_target = if let Some(rest) = braced.strip_prefix("ARGUMENTS:-") {
            Some(("ARGUMENTS", rest, "ARGUMENTS:-".len()))
        } else if let Some(rest) = braced.strip_prefix("@:-") {
            Some(("@", rest, "@:-".len()))
        } else {
            let digits = leading_digits(braced);
            if !digits.is_empty() {
                braced[digits.len()..]
                    .strip_prefix(":-")
                    .map(|rest| (digits, rest, digits.len() + 2))
            } else {
                None
            }
        };
        if let Some((target, rest, prefix_len)) = default_target
            && let Some(close) = rest.find('}')
        {
            let default_value = &rest[..close];
            let value = if target == "@" || target == "ARGUMENTS" {
                (!all_args.is_empty()).then_some(all_args)
            } else {
                positional_arg(target, args).filter(|val| !val.is_empty())
            };
            let replacement = value.unwrap_or(default_value).to_owned();
            return Some((1 + prefix_len + close + 1, replacement));
        }

        // 2. `\$\{@:(\d+)(?::(\d+))?\}`
        if let Some(after_at) = braced.strip_prefix("@:") {
            let start_digits = leading_digits(after_at);
            if !start_digits.is_empty() {
                let after_start = &after_at[start_digits.len()..];
                if after_start.starts_with('}') {
                    let start = start_digits
                        .parse::<usize>()
                        .unwrap_or(usize::MAX)
                        .saturating_sub(1);
                    let sliced = args.get(start..).unwrap_or(&[]).join(" ");
                    return Some((1 + 2 + start_digits.len() + 1, sliced));
                }
                if let Some(after_colon) = after_start.strip_prefix(':') {
                    let len_digits = leading_digits(after_colon);
                    if !len_digits.is_empty() && after_colon[len_digits.len()..].starts_with('}') {
                        let start = start_digits
                            .parse::<usize>()
                            .unwrap_or(usize::MAX)
                            .saturating_sub(1);
                        let length = len_digits.parse::<usize>().unwrap_or(usize::MAX);
                        let slice = args.get(start..).unwrap_or(&[]);
                        let end = slice.len().min(length);
                        let sliced = slice[..end].join(" ");
                        return Some((
                            1 + 2 + start_digits.len() + 1 + len_digits.len() + 1,
                            sliced,
                        ));
                    }
                }
            }
        }

        return None;
    }

    // 3. `\$(ARGUMENTS|@|\d+)`
    if after_dollar.starts_with("ARGUMENTS") {
        return Some(("ARGUMENTS".len(), all_args.to_owned()));
    }
    if after_dollar.starts_with('@') {
        return Some((1, all_args.to_owned()));
    }
    let digits = leading_digits(after_dollar);
    if !digits.is_empty() {
        let replacement = positional_arg(digits, args).unwrap_or("").to_owned();
        return Some((digits.len(), replacement));
    }

    None
}

fn truncate_utf16(text: &str, max_units: usize) -> String {
    let units: Vec<u16> = text.encode_utf16().collect();
    if units.len() <= max_units {
        return text.to_owned();
    }
    let prefix = String::from_utf16_lossy(&units[..max_units]);
    format!("{prefix}...")
}

fn load_template_from_file(
    path: &Path,
    source: &str,
) -> (Option<PromptTemplate>, Vec<PromptTemplateDiagnostic>) {
    let mut diagnostics = Vec::new();
    let raw_content = match read_text_file(path, MAX_PROMPT_TEMPLATE_FILE_BYTES) {
        Ok(Some(content)) => content,
        Ok(None) => {
            diagnostics.push(PromptTemplateDiagnostic::warning(
                "failed to read prompt template file",
                path,
            ));
            return (None, diagnostics);
        }
        Err(error) => {
            diagnostics.push(PromptTemplateDiagnostic::warning(error.to_string(), path));
            return (None, diagnostics);
        }
    };

    let (frontmatter, body) = match parse_frontmatter(&raw_content) {
        Ok(parsed) => parsed,
        Err(message) => {
            diagnostics.push(PromptTemplateDiagnostic::warning(message, path));
            return (None, diagnostics);
        }
    };

    let file_name = path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    let name = file_name
        .strip_suffix(".md")
        .unwrap_or(&file_name)
        .to_owned();

    let mut description = frontmatter
        .get("description")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_owned();
    if description.is_empty()
        && let Some(first_line) = body.split('\n').find(|line| !trim(line).is_empty())
    {
        description = truncate_utf16(first_line, 60);
    }

    let argument_hint = frontmatter
        .get("argument-hint")
        .and_then(Value::as_str)
        .filter(|hint| !hint.is_empty())
        .map(str::to_owned);

    let template = PromptTemplate {
        name,
        description,
        argument_hint,
        content: body,
        source: source.to_owned(),
        file_path: path.to_string_lossy().into_owned(),
    };
    (Some(template), diagnostics)
}

/// Which scan rules a directory gets.
#[derive(Clone, Copy, PartialEq, Eq)]
enum DirScan {
    /// A default directory: Pi's `collectAutoPromptEntries`, which skips
    /// dot-files, `node_modules`, and ignored files.
    Auto,
    /// An explicit `--prompt-template` directory: every `.md` file.
    Explicit,
}

fn load_templates_from_dir(dir: &Path, source: &str, scan: DirScan) -> LoadPromptTemplatesResult {
    let mut result = LoadPromptTemplatesResult::default();
    if !dir.exists() {
        return result;
    }
    let Ok(read_dir) = fs::read_dir(dir) else {
        return result;
    };
    let mut entries: Vec<fs::DirEntry> = read_dir.flatten().collect();
    entries.sort_by_key(fs::DirEntry::file_name);
    let mut rules = IgnoreRules::default();
    if scan == DirScan::Auto {
        rules.add_dir(dir, dir);
    }

    for entry in entries {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if scan == DirScan::Auto
            && (name.starts_with('.')
                || name == "node_modules"
                || rules.ignores(Path::new(name.as_ref()), false))
        {
            continue;
        }
        if !name.ends_with(".md") {
            continue;
        }
        let full_path = entry.path();
        let is_file = match entry.file_type() {
            Ok(ft) if ft.is_symlink() => fs::metadata(&full_path).is_ok_and(|m| m.is_file()),
            Ok(ft) => ft.is_file(),
            Err(_) => false,
        };
        if !is_file {
            continue;
        }
        let (template, diagnostics) = load_template_from_file(&full_path, source);
        result.templates.extend(template);
        result.diagnostics.extend(diagnostics);
    }

    result
}

fn resolve_against(input: &str, cwd: &Path) -> PathBuf {
    let normalized = normalize_path(trim(input));
    let joined = if normalized.is_absolute() {
        normalized
    } else {
        cwd.join(normalized)
    };
    lexical_normalize(&joined)
}

struct LoadedTemplates {
    result: LoadPromptTemplatesResult,
    real_paths: HashSet<PathBuf>,
    collisions: Vec<PromptTemplateDiagnostic>,
}

impl LoadedTemplates {
    fn add(&mut self, loaded: LoadPromptTemplatesResult) {
        self.result.diagnostics.extend(loaded.diagnostics);
        for template in loaded.templates {
            let real = fs::canonicalize(&template.file_path)
                .unwrap_or_else(|_| PathBuf::from(&template.file_path));
            if self.real_paths.contains(&real) {
                continue;
            }
            match self
                .result
                .templates
                .iter()
                .find(|existing| existing.name == template.name)
            {
                Some(existing) => self.collisions.push(PromptTemplateDiagnostic {
                    kind: PromptTemplateDiagnosticKind::Collision,
                    message: format!("name \"/{}\" collision", template.name),
                    path: template.file_path.clone(),
                    collision: Some(PromptTemplateCollision {
                        name: template.name.clone(),
                        winner_path: existing.file_path.clone(),
                        loser_path: template.file_path.clone(),
                    }),
                }),
                None => {
                    self.real_paths.insert(real);
                    self.result.templates.push(template);
                }
            }
        }
    }
}

/// Pi's `loadPromptTemplates` and `dedupePrompts`: load prompt templates from
/// `<cwd>/.bake/prompts`, `<agent_dir>/prompts`, and explicit
/// `prompt_paths`, deduplicating by template name (first wins).
pub fn load_prompt_templates(options: &LoadPromptTemplatesOptions) -> LoadPromptTemplatesResult {
    let resolved_cwd = resolve_path(Path::new(&options.cwd));
    let resolved_agent_dir = resolve_path(&options.agent_dir);
    let mut loaded = LoadedTemplates {
        result: LoadPromptTemplatesResult::default(),
        real_paths: HashSet::new(),
        collisions: Vec::new(),
    };

    if options.include_defaults {
        // Pi's `resourcePrecedenceRank`: project before user.
        let project_dir = resolved_cwd.join(".bake").join("prompts");
        let global_dir = resolved_agent_dir.join("prompts");
        loaded.add(load_templates_from_dir(
            &project_dir,
            "project",
            DirScan::Auto,
        ));
        loaded.add(load_templates_from_dir(&global_dir, "user", DirScan::Auto));
    }

    for raw in &options.prompt_paths {
        let resolved = resolve_against(raw, &resolved_cwd);
        if !resolved.exists() {
            let path = resolved.to_string_lossy().into_owned();
            if !loaded.result.diagnostics.iter().any(|d| d.path == path) {
                loaded.result.diagnostics.push(PromptTemplateDiagnostic {
                    kind: PromptTemplateDiagnosticKind::Error,
                    message: "Prompt template path does not exist".to_owned(),
                    path,
                    collision: None,
                });
            }
            continue;
        }
        match fs::metadata(&resolved) {
            Ok(metadata) if metadata.is_dir() => {
                loaded.add(load_templates_from_dir(
                    &resolved,
                    "path",
                    DirScan::Explicit,
                ));
            }
            Ok(metadata) if metadata.is_file() && resolved.to_string_lossy().ends_with(".md") => {
                let (template, diagnostics) = load_template_from_file(&resolved, "path");
                loaded.add(LoadPromptTemplatesResult {
                    templates: template.into_iter().collect(),
                    diagnostics,
                });
            }
            Ok(_) => {}
            Err(error) => loaded
                .result
                .diagnostics
                .push(PromptTemplateDiagnostic::warning(
                    error.to_string(),
                    &resolved,
                )),
        }
    }

    let LoadedTemplates {
        mut result,
        collisions,
        ..
    } = loaded;
    result.diagnostics.extend(collisions);
    result
}

/// Pi's `expandPromptTemplate`: expand `/name args` if `name` matches a
/// loaded prompt template, or return `text` unchanged.
pub fn expand_prompt_template(text: &str, templates: &[PromptTemplate]) -> String {
    let Some(rest) = text.strip_prefix('/') else {
        return text.to_owned();
    };
    if rest.is_empty() || rest.chars().next().is_some_and(is_whitespace) {
        return text.to_owned();
    }
    let name_len = rest
        .char_indices()
        .find(|(_, ch)| is_whitespace(*ch))
        .map_or(rest.len(), |(idx, _)| idx);
    let template_name = &rest[..name_len];
    let after_name = &rest[name_len..];
    let args_start = after_name
        .char_indices()
        .find(|(_, ch)| !is_whitespace(*ch))
        .map_or(after_name.len(), |(idx, _)| idx);
    let args_string = &after_name[args_start..];

    let Some(template) = templates.iter().find(|t| t.name == template_name) else {
        return text.to_owned();
    };
    let args = parse_command_args(args_string);
    substitute_args(&template.content, &args)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::TempDir;

    fn args(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| (*s).to_owned()).collect()
    }

    // Ported from Pi `packages/coding-agent/test/prompt-templates.test.ts`.
    #[test]
    fn substitute_args_handles_all_placeholders_and_never_recurses() {
        assert_eq!(
            substitute_args("Test: $ARGUMENTS", &args(&["a", "b", "c"])),
            "Test: a b c"
        );
        assert_eq!(
            substitute_args("Test: $@", &args(&["a", "b", "c"])),
            "Test: a b c"
        );
        assert_eq!(
            substitute_args("Test: $@", &args(&["foo", "bar", "baz"])),
            substitute_args("Test: $ARGUMENTS", &args(&["foo", "bar", "baz"]))
        );

        // Argument values containing patterns remain literal.
        assert_eq!(
            substitute_args("$ARGUMENTS", &args(&["$1", "$ARGUMENTS"])),
            "$1 $ARGUMENTS"
        );
        assert_eq!(substitute_args("$@", &args(&["$100", "$1"])), "$100 $1");
        assert_eq!(
            substitute_args("$ARGUMENTS", &args(&["$100", "$1"])),
            "$100 $1"
        );

        assert_eq!(
            substitute_args("$1: $ARGUMENTS", &args(&["prefix", "a", "b"])),
            "prefix: prefix a b"
        );
        assert_eq!(
            substitute_args("$1: $@", &args(&["prefix", "a", "b"])),
            "prefix: prefix a b"
        );
        assert_eq!(substitute_args("Test: $ARGUMENTS", &[]), "Test: ");
        assert_eq!(substitute_args("Test: $@", &[]), "Test: ");
        assert_eq!(substitute_args("Test: $1", &[]), "Test: ");
        assert_eq!(
            substitute_args("$ARGUMENTS and $ARGUMENTS", &args(&["a", "b"])),
            "a b and a b"
        );
        assert_eq!(
            substitute_args("$@ and $@", &args(&["a", "b"])),
            "a b and a b"
        );
        assert_eq!(
            substitute_args("$@ and $ARGUMENTS", &args(&["a", "b"])),
            "a b and a b"
        );
        assert_eq!(
            substitute_args("$1 $2: $ARGUMENTS", &args(&["arg100", "@user"])),
            "arg100 @user: arg100 @user"
        );
        assert_eq!(
            substitute_args("$1 $2 $3 $4 $5", &args(&["a", "b"])),
            "a b   "
        );
        assert_eq!(
            substitute_args("$ARGUMENTS", &args(&["日本語", "🎉", "café"])),
            "日本語 🎉 café"
        );
        assert_eq!(
            substitute_args("$1 $2", &args(&["line1\nline2", "tab\tthere"])),
            "line1\nline2 tab\tthere"
        );
        assert_eq!(substitute_args("$1$2", &args(&["a", "b"])), "ab");
        assert_eq!(
            substitute_args("$ARGUMENTS", &args(&["first arg", "second arg"])),
            "first arg second arg"
        );
        assert_eq!(
            substitute_args("Test: $ARGUMENTS", &args(&["only"])),
            "Test: only"
        );
        assert_eq!(substitute_args("Test: $@", &args(&["only"])), "Test: only");
        assert_eq!(substitute_args("$0", &args(&["a", "b"])), "");
        assert_eq!(substitute_args("$1.5", &args(&["a"])), "a.5");
        assert_eq!(
            substitute_args("pre$ARGUMENTS", &args(&["a", "b"])),
            "prea b"
        );
        assert_eq!(substitute_args("pre$@", &args(&["a", "b"])), "prea b");
        assert_eq!(
            substitute_args("$ARGUMENTS", &args(&["a", "", "c"])),
            "a  c"
        );
        assert_eq!(
            substitute_args("$ARGUMENTS", &args(&["  leading  ", "trailing  "])),
            "  leading   trailing  "
        );
        assert_eq!(
            substitute_args("Prefix $ARGUMENTS suffix", &args(&["ARGUMENTS"])),
            "Prefix ARGUMENTS suffix"
        );
        assert_eq!(
            substitute_args("$A $$ $ $ARGS", &args(&["a"])),
            "$A $$ $ $ARGS"
        );
        assert_eq!(
            substitute_args("$arguments $Arguments $ARGUMENTS", &args(&["a", "b"])),
            "$arguments $Arguments a b"
        );
        let fifteen: Vec<String> = (0..15).map(|i| format!("val{i}")).collect();
        assert_eq!(substitute_args("$10 $12 $15", &fifteen), "val9 val11 val14");
        assert_eq!(substitute_args("Price: \\$100", &[]), "Price: \\");
        assert_eq!(
            substitute_args("$1: $@ ($ARGUMENTS)", &args(&["first", "second", "third"])),
            "first: first second third (first second third)"
        );
        assert_eq!(
            substitute_args("Just plain text", &args(&["a", "b"])),
            "Just plain text"
        );
        assert_eq!(
            substitute_args("$1 $2 $@", &args(&["a", "b", "c"])),
            "a b a b c"
        );
    }

    #[test]
    fn substitute_args_supports_positional_defaults_and_array_slicing() {
        assert_eq!(
            substitute_args("List exactly ${1:-7} next steps", &[]),
            "List exactly 7 next steps"
        );
        assert_eq!(
            substitute_args("${@:-default}\n${ARGUMENTS:-default}", &[]),
            "default\ndefault"
        );
        assert_eq!(
            substitute_args(
                "${@:-default}\n${ARGUMENTS:-default}",
                &args(&["This", "would", "be", "the", "arguments"])
            ),
            "This would be the arguments\nThis would be the arguments"
        );
        assert_eq!(
            substitute_args("List exactly ${1:-7} next steps", &args(&["3"])),
            "List exactly 3 next steps"
        );
        assert_eq!(
            substitute_args("Mode: ${1:-brief}", &args(&[""])),
            "Mode: brief"
        );
        assert_eq!(substitute_args("${1:-7} ${2:-brief}", &[]), "7 brief");
        assert_eq!(
            substitute_args("${1:-7} ${2:-brief}", &args(&["3"])),
            "3 brief"
        );
        assert_eq!(
            substitute_args("${1:-7} ${2:-brief}", &args(&["3", "verbose"])),
            "3 verbose"
        );
        assert_eq!(
            substitute_args("${1:-7}", &args(&["$ARGUMENTS"])),
            "$ARGUMENTS"
        );
        assert_eq!(substitute_args("${1:-7}", &args(&["$1"])), "$1");
        assert_eq!(substitute_args("${1:-$ARGUMENTS}", &args(&["a", "b"])), "a");
        assert_eq!(
            substitute_args("${3:-$ARGUMENTS}", &args(&["a", "b"])),
            "$ARGUMENTS"
        );
        assert_eq!(substitute_args("${1:-seven steps}", &[]), "seven steps");
        assert_eq!(
            substitute_args("${3:-fallback}", &args(&["a", "b"])),
            "fallback"
        );
        assert_eq!(
            substitute_args("$1 ${2:-x} $ARGUMENTS", &args(&["a"])),
            "a x a"
        );

        // Slicing
        assert_eq!(
            substitute_args("${@:2}", &args(&["a", "b", "c", "d"])),
            "b c d"
        );
        assert_eq!(substitute_args("${@:1}", &args(&["a", "b", "c"])), "a b c");
        assert_eq!(
            substitute_args("${@:3}", &args(&["a", "b", "c", "d"])),
            "c d"
        );
        assert_eq!(
            substitute_args("${@:2:2}", &args(&["a", "b", "c", "d"])),
            "b c"
        );
        assert_eq!(substitute_args("${@:1:1}", &args(&["a", "b", "c"])), "a");
        assert_eq!(
            substitute_args("${@:3:1}", &args(&["a", "b", "c", "d"])),
            "c"
        );
        assert_eq!(
            substitute_args("${@:2:3}", &args(&["a", "b", "c", "d", "e"])),
            "b c d"
        );
        assert_eq!(substitute_args("${@:99}", &args(&["a", "b"])), "");
        assert_eq!(substitute_args("${@:10:5}", &args(&["a", "b"])), "");
        assert_eq!(substitute_args("${@:2:0}", &args(&["a", "b", "c"])), "");
        assert_eq!(substitute_args("${@:2:99}", &args(&["a", "b", "c"])), "b c");
        assert_eq!(
            substitute_args("${@:2} vs $@", &args(&["a", "b", "c"])),
            "b c vs a b c"
        );
        assert_eq!(
            substitute_args("First: ${@:1:1}, All: $@", &args(&["x", "y", "z"])),
            "First: x, All: x y z"
        );
        assert_eq!(
            substitute_args("${@:1}", &args(&["${@:2}", "test"])),
            "${@:2} test"
        );
        assert_eq!(substitute_args("${@:0}", &args(&["a", "b", "c"])), "a b c");
        assert_eq!(substitute_args("${@:2}", &[]), "");
        assert_eq!(
            substitute_args("prefix${@:2}suffix", &args(&["a", "b", "c"])),
            "prefixb csuffix"
        );
    }

    #[test]
    fn parse_command_args_and_expand_prompt_template_follow_pi() {
        assert_eq!(parse_command_args("a b c"), args(&["a", "b", "c"]));
        assert_eq!(
            parse_command_args("\"first arg\" second"),
            args(&["first arg", "second"])
        );
        assert_eq!(
            parse_command_args("'first arg' second"),
            args(&["first arg", "second"])
        );
        assert_eq!(
            parse_command_args("\"double\" 'single' \"double again\""),
            args(&["double", "single", "double again"])
        );
        assert_eq!(parse_command_args(""), Vec::<String>::new());
        assert_eq!(parse_command_args("a  b   c"), args(&["a", "b", "c"]));
        assert_eq!(parse_command_args("a\tb\tc"), args(&["a", "b", "c"]));
        assert_eq!(parse_command_args("\"\" \" \""), args(&[" "]));
        assert_eq!(
            parse_command_args("$100 @user #tag"),
            args(&["$100", "@user", "#tag"])
        );
        assert_eq!(
            parse_command_args("日本語 🎉 café"),
            args(&["日本語", "🎉", "café"])
        );
        assert_eq!(
            parse_command_args("\"line1\nline2\" second"),
            args(&["line1\nline2", "second"])
        );
        assert_eq!(
            parse_command_args("label-2\n\nHere is some description #2."),
            args(&["label-2", "Here", "is", "some", "description", "#2."])
        );
        assert_eq!(parse_command_args("a\n\n\tb  c"), args(&["a", "b", "c"]));
        assert_eq!(
            parse_command_args("\"quoted \\\"text\\\"\""),
            args(&["quoted \\text\\"])
        );

        let templates = vec![PromptTemplate {
            name: "arg-test".into(),
            description: "test".into(),
            argument_hint: None,
            content: "- arg1: $1\n- rest: ${@:2}".into(),
            source: "path".into(),
            file_path: "/tmp/arg-test.md".into(),
        }];
        assert_eq!(
            expand_prompt_template(
                "/arg-test label-2\n\nHere is some description #2.",
                &templates
            ),
            "- arg1: label-2\n- rest: Here is some description #2."
        );
        let templates2 = vec![PromptTemplate {
            name: "arg-test".into(),
            description: "test".into(),
            argument_hint: None,
            content: "arg1: $1".into(),
            source: "path".into(),
            file_path: "/tmp/arg-test.md".into(),
        }];
        assert_eq!(
            expand_prompt_template("/arg-test\nlabel-2", &templates2),
            "arg1: label-2"
        );
    }

    #[test]
    fn load_prompt_templates_parses_frontmatter_diagnostics_and_collisions() {
        let dir = TempDir::new("bake-prompt-templates");
        let prompts_dir = dir.path().join("prompts");
        fs::create_dir_all(&prompts_dir).expect("prompts dir");

        fs::write(
            prompts_dir.join("pr.md"),
            "---\ndescription: Review PRs from URLs with structured issue and code analysis\nargument-hint: \"<PR-URL>\"\n---\nYou are given one or more GitHub PR URLs: $@",
        )
        .expect("pr.md");
        fs::write(
            prompts_dir.join("cl.md"),
            "---\ndescription: Audit changelog entries before release\n---\nAudit changelog entries for all commits since the last release.",
        )
        .expect("cl.md");
        fs::write(
            prompts_dir.join("empty-hint.md"),
            "---\ndescription: A command with empty hint\nargument-hint: \"\"\n---\nDo something",
        )
        .expect("empty-hint.md");
        fs::write(
            prompts_dir.join("invalid.md"),
            "---\ndescription: Broken: unquoted colon\n---\nDo something.\n",
        )
        .expect("invalid.md");
        fs::write(prompts_dir.join("valid.md"), "Valid prompt content.").expect("valid.md");

        let second_dir = dir.path().join("second");
        fs::create_dir_all(&second_dir).expect("second dir");
        fs::write(second_dir.join("pr.md"), "Duplicate pr template").expect("dup pr.md");

        let loaded = load_prompt_templates(&LoadPromptTemplatesOptions {
            cwd: dir.path().to_string_lossy().into_owned(),
            agent_dir: dir.path().join("agent"),
            prompt_paths: vec![
                prompts_dir.to_string_lossy().into_owned(),
                second_dir.to_string_lossy().into_owned(),
            ],
            include_defaults: false,
        });

        let names: Vec<&str> = loaded.templates.iter().map(|t| t.name.as_str()).collect();
        assert_eq!(names, ["cl", "empty-hint", "pr", "valid"]);

        let pr = loaded.templates.iter().find(|t| t.name == "pr").unwrap();
        assert_eq!(pr.argument_hint.as_deref(), Some("<PR-URL>"));
        assert_eq!(
            pr.description,
            "Review PRs from URLs with structured issue and code analysis"
        );

        let cl = loaded.templates.iter().find(|t| t.name == "cl").unwrap();
        assert_eq!(cl.argument_hint, None);

        let empty_hint = loaded
            .templates
            .iter()
            .find(|t| t.name == "empty-hint")
            .unwrap();
        assert_eq!(empty_hint.argument_hint, None);

        let valid = loaded.templates.iter().find(|t| t.name == "valid").unwrap();
        assert_eq!(valid.description, "Valid prompt content.");

        assert_eq!(loaded.diagnostics.len(), 2);
        assert_eq!(
            loaded.diagnostics[0].kind,
            PromptTemplateDiagnosticKind::Warning
        );
        assert!(
            loaded.diagnostics[0].path.ends_with("invalid.md"),
            "{:?}",
            loaded.diagnostics[0]
        );
        assert!(
            loaded.diagnostics[0]
                .message
                .contains("Frontmatter is not valid YAML at line 1, column 20"),
            "{:?}",
            loaded.diagnostics[0]
        );
        assert_eq!(
            loaded.diagnostics[1].kind,
            PromptTemplateDiagnosticKind::Collision
        );
        assert_eq!(loaded.diagnostics[1].message, "name \"/pr\" collision");
    }

    // Pi `package-manager.ts` `resourcePrecedenceRank` and
    // `collectAutoPromptEntries`, and `resource-loader.ts`'s missing-path error.
    #[test]
    fn default_directories_rank_project_first_skip_hidden_and_ignored_files() {
        let dir = TempDir::new("bake-prompt-templates-defaults");
        let cwd = dir.path().join("project");
        let agent_dir = dir.path().join("agent");
        let project_prompts = cwd.join(".bake").join("prompts");
        let user_prompts = agent_dir.join("prompts");
        fs::create_dir_all(&project_prompts).expect("project prompts");
        fs::create_dir_all(user_prompts.join("node_modules")).expect("user prompts");
        fs::write(project_prompts.join("review.md"), "Project review").expect("project review");
        fs::write(user_prompts.join("review.md"), "User review").expect("user review");
        fs::write(user_prompts.join("user-only.md"), "User only").expect("user-only");
        fs::write(project_prompts.join(".draft.md"), "Hidden").expect("hidden");
        fs::write(project_prompts.join("skipped.md"), "Ignored").expect("ignored");
        fs::write(project_prompts.join(".gitignore"), "skipped.md\n").expect("gitignore");
        let explicit = dir.path().join("explicit");
        fs::create_dir_all(&explicit).expect("explicit");
        fs::write(explicit.join(".dotted.md"), "Explicit dot-file").expect("dotted");
        fs::write(explicit.join("review.md"), "Explicit review").expect("explicit review");

        let loaded = load_prompt_templates(&LoadPromptTemplatesOptions {
            cwd: cwd.to_string_lossy().into_owned(),
            agent_dir: agent_dir.clone(),
            prompt_paths: vec![
                explicit.to_string_lossy().into_owned(),
                "missing.md".to_owned(),
                "missing.md".to_owned(),
            ],
            include_defaults: true,
        });

        let loaded_names: Vec<(&str, &str)> = loaded
            .templates
            .iter()
            .map(|t| (t.name.as_str(), t.source.as_str()))
            .collect();
        assert_eq!(
            loaded_names,
            [
                ("review", "project"),
                ("user-only", "user"),
                (".dotted", "path"),
            ]
        );
        assert_eq!(
            expand_prompt_template("/review", &loaded.templates),
            "Project review"
        );

        let missing = cwd.join("missing.md").to_string_lossy().into_owned();
        let errors: Vec<&PromptTemplateDiagnostic> = loaded
            .diagnostics
            .iter()
            .filter(|d| d.kind == PromptTemplateDiagnosticKind::Error)
            .collect();
        assert_eq!(errors.len(), 1, "{:?}", loaded.diagnostics);
        assert_eq!(errors[0].message, "Prompt template path does not exist");
        assert_eq!(errors[0].path, missing);
        let losers: Vec<&str> = loaded
            .diagnostics
            .iter()
            .filter_map(|d| d.collision.as_ref())
            .map(|c| c.loser_path.as_str())
            .collect();
        assert_eq!(losers.len(), 2, "{:?}", loaded.diagnostics);
        assert!(
            Path::new(losers[0]).ends_with(Path::new("agent").join("prompts").join("review.md")),
            "{losers:?}"
        );
        assert!(
            Path::new(losers[1]).ends_with(Path::new("explicit").join("review.md")),
            "{losers:?}"
        );
    }

    #[test]
    fn expand_prompt_template_leaves_unknown_names_and_bare_slashes() {
        let templates = vec![PromptTemplate {
            name: "known".into(),
            description: String::new(),
            argument_hint: None,
            content: "Known $1".into(),
            source: "path".into(),
            file_path: "/known.md".into(),
        }];
        assert_eq!(expand_prompt_template("/", &templates), "/");
        assert_eq!(expand_prompt_template("/ known", &templates), "/ known");
        assert_eq!(
            expand_prompt_template("/unknown a", &templates),
            "/unknown a"
        );
        assert_eq!(expand_prompt_template("known a", &templates), "known a");
        assert_eq!(expand_prompt_template("/known a", &templates), "Known a");
    }
}
