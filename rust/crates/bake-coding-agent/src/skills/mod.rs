//! Agent Skills: discovery, validation, the system prompt's skills list, and
//! `/skill:name` expansion.
//!
//! Ported from Pi `packages/coding-agent/src/core/skills.ts` (v1.2.0,
//! revision `06a466935536f8c9bcacb2d5efc0fc021be1bdff`, unchanged since
//! v1.1.0), `src/utils/frontmatter.ts`, the `.agents` skill discovery of
//! `src/core/package-manager.ts` (`collectSkillEntries`,
//! `collectAncestorAgentsSkillDirs`), and `_expandSkillCommand` from
//! `src/core/agent-session.ts`.
//!
//! # Where skills come from
//!
//! Bake reads the universal `.agents` locations only. With the defaults on,
//! [`load_skills`] reads, in this order, the first of each name winning:
//!
//! 1. every `.agents/skills` from the working directory up to the git
//!    repository root, or to the filesystem root outside a repository,
//!    nearest first;
//! 2. the user's `~/.agents/skills`;
//! 3. each explicit path (`--skill`), a skill file or a directory.
//!
//! In a `.agents/skills` root, a directory holding `SKILL.md` is one skill
//! and is not searched further; other Markdown files count only below the
//! root. An explicit directory is read as Pi reads its own skill
//! directories: Markdown files directly in it count too. `.gitignore`,
//! `.ignore`, and `.fdignore` files inside a root apply below them. A
//! Markdown file that is not `SKILL.md` is a skill only when its
//! frontmatter has a description.
//!
//! # Deviations from Pi
//!
//! - **Roots.** Pi also reads `~/.pi/agent/skills` and the project's
//!   `.pi/skills`, and the `skills` paths of its settings and packages;
//!   Bake reads neither a Bake-home nor a `.bake` skills directory.
//! - **No project trust.** Pi loads a project's `.agents/skills` only once
//!   the project is trusted, which print mode without `--approve` never is.
//!   Bake loads them like the project's `AGENTS.md`: project trust is not
//!   ported.
//! - **Bounds.** A directory reached twice through links is searched once,
//!   and the search stops [`MAX_DEPTH`] levels below a root, where Pi
//!   follows a link cycle until the system refuses the path. A skill file
//!   larger than [`MAX_SKILL_FILE_BYTES`] is skipped with a warning, and its
//!   frontmatter is read by the crate's bounded YAML reader, so a parse
//!   error names the line and column but not the parser's own words.
//! - **Diagnostics** are returned; `bake-rs -p` does not print them, as Pi's
//!   print mode does not.

use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};

use ignore::gitignore::{Gitignore, GitignoreBuilder};
use serde_json::Value;

use crate::auth_storage::{read_text_file, strip_bom};
use crate::session::paths::{lexical_normalize, normalize_path, resolve_path};
use crate::tools::js::trim;

#[cfg(test)]
mod tests;

/// The longest skill name the Agent Skills specification allows, in UTF-16
/// code units.
pub const MAX_NAME_LENGTH: usize = 64;
/// The longest description the specification allows, in UTF-16 code units.
pub const MAX_DESCRIPTION_LENGTH: usize = 1024;
/// The largest skill file read, in bytes.
pub const MAX_SKILL_FILE_BYTES: u64 = 1024 * 1024;
/// How many directory levels below a root the search descends.
pub const MAX_DEPTH: usize = 32;

const IGNORE_FILE_NAMES: [&str; 3] = [".gitignore", ".ignore", ".fdignore"];
const SKILL_FILE: &str = "SKILL.md";

/// A loaded skill, Pi's `Skill`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Skill {
    /// The frontmatter `name`, else the skill directory's name.
    pub name: String,
    /// The frontmatter `description`.
    pub description: String,
    /// The skill file.
    pub file_path: String,
    /// The directory holding the skill file, which relative references in
    /// it resolve against.
    pub base_dir: String,
    /// Where it was found: `project`, `user`, `path`, or a caller's label.
    pub source: String,
    /// `disable-model-invocation: true`: left out of the system prompt and
    /// reachable only through `/skill:name`.
    pub disable_model_invocation: bool,
}

/// A diagnostic's kind, Pi's `ResourceDiagnostic.type` for skills.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SkillDiagnosticKind {
    /// A problem with one file or path.
    Warning,
    /// A second skill with a loaded skill's name, which was skipped.
    Collision,
}

/// The two files of a name collision.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkillCollision {
    /// The shared name.
    pub name: String,
    /// The skill that was kept.
    pub winner_path: String,
    /// The skill that was skipped.
    pub loser_path: String,
}

/// A problem found while loading skills, Pi's `ResourceDiagnostic`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkillDiagnostic {
    /// Its kind.
    pub kind: SkillDiagnosticKind,
    /// Pi's message.
    pub message: String,
    /// The file or path concerned.
    pub path: String,
    /// The collision, for [`SkillDiagnosticKind::Collision`].
    pub collision: Option<SkillCollision>,
}

impl SkillDiagnostic {
    fn warning(message: impl Into<String>, path: &Path) -> Self {
        Self {
            kind: SkillDiagnosticKind::Warning,
            message: message.into(),
            path: path.to_string_lossy().into_owned(),
            collision: None,
        }
    }
}

/// Skills and the problems found loading them, Pi's `LoadSkillsResult`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LoadSkillsResult {
    /// The skills, in load order.
    pub skills: Vec<Skill>,
    /// Problems, collisions last.
    pub diagnostics: Vec<SkillDiagnostic>,
}

/// How a root's Markdown files count, Pi's `SkillDiscoveryMode`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Discovery {
    /// Pi's own skill directories and explicit directories: Markdown files
    /// directly in the root.
    Root,
    /// `.agents/skills`: Markdown files below the root.
    Agents,
}

/// The ignore rules in force for a directory: Pi's prefixed patterns,
/// matched against paths relative to the root.
#[derive(Clone, Default)]
struct IgnoreRules {
    lines: Vec<String>,
    matcher: Option<Gitignore>,
}

impl IgnoreRules {
    /// Pi's `addIgnoreRules`: the patterns of `dir`'s ignore files,
    /// prefixed with `dir`'s path below the root.
    fn add_dir(&mut self, dir: &Path, root: &Path) {
        let prefix = match dir.strip_prefix(root) {
            Ok(relative) if relative.as_os_str().is_empty() => String::new(),
            Ok(relative) => format!("{}/", to_posix(relative)),
            Err(_) => String::new(),
        };
        let mut added = false;
        for name in IGNORE_FILE_NAMES {
            let Ok(Some(content)) = read_text_file(&dir.join(name), MAX_SKILL_FILE_BYTES) else {
                continue;
            };
            for line in content.split('\n') {
                let line = line.strip_suffix('\r').unwrap_or(line);
                if let Some(pattern) = prefix_ignore_pattern(line, &prefix) {
                    self.lines.push(pattern);
                    added = true;
                }
            }
        }
        if added {
            let mut builder = GitignoreBuilder::new("");
            for line in &self.lines {
                // An invalid glob is skipped, as the `ignore` package does.
                let _ = builder.add_line(None, line);
            }
            self.matcher = builder.build().ok();
        }
    }

    /// Pi's `ig.ignores(relative)`, which also checks the parents.
    fn ignores(&self, relative: &Path, is_dir: bool) -> bool {
        match &self.matcher {
            Some(matcher) if !relative.has_root() => matcher
                .matched_path_or_any_parents(relative, is_dir)
                .is_ignore(),
            _ => false,
        }
    }
}

fn to_posix(path: &Path) -> String {
    path.components()
        .map(|component| component.as_os_str().to_string_lossy().into_owned())
        .collect::<Vec<_>>()
        .join("/")
}

/// Pi's `prefixIgnorePattern`.
fn prefix_ignore_pattern(line: &str, prefix: &str) -> Option<String> {
    let trimmed = trim(line);
    if trimmed.is_empty() || (trimmed.starts_with('#') && !trimmed.starts_with("\\#")) {
        return None;
    }
    let (negated, pattern) = if let Some(rest) = line.strip_prefix('!') {
        (true, rest)
    } else if line.starts_with("\\!") {
        (false, &line[1..])
    } else {
        (false, line)
    };
    let pattern = pattern.strip_prefix('/').unwrap_or(pattern);
    let prefixed = format!("{prefix}{pattern}");
    Some(if negated {
        format!("!{prefixed}")
    } else {
        prefixed
    })
}

/// A directory entry's kind, links followed as Pi's `statSync` follows them.
fn entry_kind(entry: &fs::DirEntry) -> Option<(bool, bool)> {
    let file_type = entry.file_type().ok()?;
    if file_type.is_symlink() {
        let metadata = fs::metadata(entry.path()).ok()?;
        Some((metadata.is_dir(), metadata.is_file()))
    } else {
        Some((file_type.is_dir(), file_type.is_file()))
    }
}

/// A directory's entries sorted by name, as Node's `readdirSync` lists
/// them on Unix; `None` when it cannot be read.
fn sorted_entries(dir: &Path) -> Option<Vec<fs::DirEntry>> {
    let mut entries: Vec<fs::DirEntry> = fs::read_dir(dir).ok()?.flatten().collect();
    entries.sort_by_key(fs::DirEntry::file_name);
    Some(entries)
}

struct Walk<'a> {
    root: &'a Path,
    discovery: Discovery,
    visited: HashSet<PathBuf>,
    files: Vec<PathBuf>,
}

impl Walk<'_> {
    /// Pi's `collectSkillEntries` and `loadSkillsFromDirInternal`.
    fn dir(&mut self, dir: &Path, rules: &IgnoreRules, depth: usize) {
        if depth > MAX_DEPTH {
            return;
        }
        let real = fs::canonicalize(dir).unwrap_or_else(|_| dir.to_path_buf());
        if !self.visited.insert(real) {
            return;
        }
        let mut rules = rules.clone();
        rules.add_dir(dir, self.root);
        let Some(entries) = sorted_entries(dir) else {
            return;
        };
        let root = self.root;
        let relative = |path: &Path| path.strip_prefix(root).unwrap_or(path).to_path_buf();

        for entry in &entries {
            if entry.file_name() != SKILL_FILE {
                continue;
            }
            let path = entry.path();
            let Some((_, is_file)) = entry_kind(entry) else {
                continue;
            };
            if is_file && !rules.ignores(&relative(&path), false) {
                self.files.push(path);
                return;
            }
        }

        let at_root = dir == self.root;
        for entry in &entries {
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if name.starts_with('.') || name == "node_modules" {
                continue;
            }
            let path = entry.path();
            let Some((is_dir, is_file)) = entry_kind(entry) else {
                continue;
            };
            let relative = relative(&path);
            if is_dir {
                if !rules.ignores(&relative, true) {
                    self.dir(&path, &rules, depth + 1);
                }
                continue;
            }
            let counts = match self.discovery {
                Discovery::Root => at_root,
                Discovery::Agents => !at_root,
            };
            if is_file && counts && name.ends_with(".md") && !rules.ignores(&relative, false) {
                self.files.push(path);
            }
        }
    }
}

/// The candidate skill files under `dir`, in Pi's order.
fn collect_skill_files(dir: &Path, discovery: Discovery) -> Vec<PathBuf> {
    if !dir.exists() {
        return Vec::new();
    }
    let mut walk = Walk {
        root: dir,
        discovery,
        visited: HashSet::new(),
        files: Vec::new(),
    };
    walk.dir(dir, &IgnoreRules::default(), 0);
    walk.files
}

/// Pi's `loadSkillsFromDir`: the skills under `dir`, read as Pi reads its
/// own skill directories, each labelled `source`.
pub fn load_skills_from_dir(dir: &Path, source: &str) -> LoadSkillsResult {
    load_files(&collect_skill_files(dir, Discovery::Root), source)
}

fn load_files(files: &[PathBuf], source: &str) -> LoadSkillsResult {
    let mut result = LoadSkillsResult::default();
    for file in files {
        let (skill, diagnostics) = load_skill_from_file(file, source);
        result.skills.extend(skill);
        result.diagnostics.extend(diagnostics);
    }
    result
}

/// Pi's `extractFrontmatter`: the YAML text, if any, and the body.
fn extract_frontmatter(content: &str) -> (Option<String>, String) {
    let normalized = strip_bom(content).replace("\r\n", "\n").replace('\r', "\n");
    if !normalized.starts_with("---") {
        return (None, normalized);
    }
    let Some(end) = normalized[3..].find("\n---").map(|index| index + 3) else {
        return (None, normalized);
    };
    // JavaScript's `slice(4, end)`: the character after `---` is skipped
    // whatever it is.
    let mut after = normalized[3..end].chars();
    after.next();
    let yaml = after.as_str().to_owned();
    let body = trim(&normalized[end + 4..]).to_owned();
    ((!yaml.is_empty()).then_some(yaml), body)
}

/// Pi's `parseFrontmatter`: the frontmatter as JSON (an empty object when
/// there is none) and the body.
pub fn parse_frontmatter(content: &str) -> Result<(Value, String), String> {
    let (yaml, body) = extract_frontmatter(content);
    let Some(yaml) = yaml else {
        return Ok((Value::Object(serde_json::Map::new()), body));
    };
    let max = usize::try_from(MAX_SKILL_FILE_BYTES).unwrap_or(usize::MAX);
    let parsed = crate::cliproxyapi::yaml::parse_document(yaml.as_bytes(), max)
        .map_err(|error| format!("Frontmatter {error}"))?;
    let parsed = if parsed.is_null() {
        Value::Object(serde_json::Map::new())
    } else {
        parsed
    };
    Ok((parsed, body))
}

/// Pi's `stripFrontmatter`.
pub fn strip_frontmatter(content: &str) -> String {
    extract_frontmatter(content).1
}

fn utf16_len(text: &str) -> usize {
    text.encode_utf16().count()
}

/// Pi's `validateName`.
fn validate_name(name: &str) -> Vec<String> {
    let mut errors = Vec::new();
    let length = utf16_len(name);
    if length > MAX_NAME_LENGTH {
        errors.push(format!(
            "name exceeds {MAX_NAME_LENGTH} characters ({length})"
        ));
    }
    if name.is_empty()
        || !name
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
    {
        errors.push(
            "name contains invalid characters (must be lowercase a-z, 0-9, hyphens only)".into(),
        );
    }
    if name.starts_with('-') || name.ends_with('-') {
        errors.push("name must not start or end with a hyphen".into());
    }
    if name.contains("--") {
        errors.push("name must not contain consecutive hyphens".into());
    }
    errors
}

/// Pi's `validateDescription`.
fn validate_description(description: Option<&str>) -> Vec<String> {
    match description {
        Some(text) if !trim(text).is_empty() => {
            let length = utf16_len(text);
            if length > MAX_DESCRIPTION_LENGTH {
                vec![format!(
                    "description exceeds {MAX_DESCRIPTION_LENGTH} characters ({length})"
                )]
            } else {
                Vec::new()
            }
        }
        _ => vec!["description is required".into()],
    }
}

/// Pi's `loadSkillFromFile`.
fn load_skill_from_file(path: &Path, source: &str) -> (Option<Skill>, Vec<SkillDiagnostic>) {
    let mut diagnostics = Vec::new();
    let declared = path.file_name().is_some_and(|name| name == SKILL_FILE);
    let content = match read_text_file(path, MAX_SKILL_FILE_BYTES) {
        Ok(Some(content)) => content,
        Ok(None) => {
            diagnostics.push(SkillDiagnostic::warning("failed to read skill file", path));
            return (None, diagnostics);
        }
        Err(error) => {
            diagnostics.push(SkillDiagnostic::warning(error.to_string(), path));
            return (None, diagnostics);
        }
    };
    let frontmatter = match parse_frontmatter(&content) {
        Ok((frontmatter, _)) => frontmatter,
        Err(message) => {
            if declared {
                diagnostics.push(SkillDiagnostic::warning(message, path));
            }
            return (None, diagnostics);
        }
    };
    let description = frontmatter.get("description").and_then(Value::as_str);
    let has_description = description.is_some_and(|text| !trim(text).is_empty());
    if !declared && !has_description {
        return (None, diagnostics);
    }
    let skill_dir = path.parent().unwrap_or(Path::new(""));
    let parent_name = skill_dir
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    for error in validate_description(description) {
        diagnostics.push(SkillDiagnostic::warning(error, path));
    }
    let name = frontmatter
        .get("name")
        .and_then(Value::as_str)
        .filter(|name| !name.is_empty())
        .map_or(parent_name, str::to_owned);
    for error in validate_name(&name) {
        diagnostics.push(SkillDiagnostic::warning(error, path));
    }
    let Some(description) = description.filter(|_| has_description) else {
        return (None, diagnostics);
    };
    let skill = Skill {
        name,
        description: description.to_owned(),
        file_path: path.to_string_lossy().into_owned(),
        base_dir: skill_dir.to_string_lossy().into_owned(),
        source: source.to_owned(),
        disable_model_invocation: frontmatter.get("disable-model-invocation")
            == Some(&Value::Bool(true)),
    };
    (Some(skill), diagnostics)
}

/// Which tool the skills list tells the model to load skill files with.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SkillReader {
    /// The `read` tool.
    Read,
    /// The `bash` tool.
    Bash,
    /// No tool by name: the reader is reachable only through another tool.
    Indirect,
}

/// Pi's `escapeXml`.
fn escape_xml(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

/// Pi's `formatSkillsForPrompt`: the skills list in the Agent Skills XML
/// form, leaving out skills with `disable-model-invocation`; empty when no
/// skill remains.
pub fn format_skills_for_prompt(skills: &[Skill], reader: SkillReader) -> String {
    let visible: Vec<&Skill> = skills
        .iter()
        .filter(|skill| !skill.disable_model_invocation)
        .collect();
    if visible.is_empty() {
        return String::new();
    }
    let mut lines = vec![
        "\n\nThe following skills provide specialized instructions for specific tasks.".to_owned(),
        match reader {
            SkillReader::Read => {
                "Use the read tool to load a skill's file when the task matches its description."
            }
            SkillReader::Bash => {
                "Use bash to load a skill's file when the task matches its description."
            }
            SkillReader::Indirect => "Load a skill's file when the task matches its description.",
        }
        .to_owned(),
        "When a skill file references a relative path, resolve it against the skill directory (parent of SKILL.md / dirname of the path) and use that absolute path in tool commands.".to_owned(),
        String::new(),
        "<available_skills>".to_owned(),
    ];
    for skill in visible {
        lines.push("  <skill>".to_owned());
        lines.push(format!("    <name>{}</name>", escape_xml(&skill.name)));
        lines.push(format!(
            "    <description>{}</description>",
            escape_xml(&skill.description)
        ));
        lines.push(format!(
            "    <location>{}</location>",
            escape_xml(&skill.file_path)
        ));
        lines.push("  </skill>".to_owned());
    }
    lines.push("</available_skills>".to_owned());
    lines.join("\n")
}

/// What [`load_skills`] reads, Pi's `LoadSkillsOptions` for Bake's roots.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LoadSkillsOptions {
    /// The working directory, where the project search starts and against
    /// which relative skill paths resolve.
    pub cwd: String,
    /// The user's home directory, which holds `.agents/skills`; `None`
    /// when it is unknown.
    pub user_home: Option<PathBuf>,
    /// Explicit skill files or directories (`--skill`).
    pub skill_paths: Vec<String>,
    /// Whether to read the `.agents/skills` roots (`--no-skills` clears
    /// it).
    pub include_defaults: bool,
}

/// Pi's `findGitRepoRoot`.
fn find_git_repo_root(start: &Path) -> Option<PathBuf> {
    let mut dir = start;
    loop {
        if dir.join(".git").exists() {
            return Some(dir.to_path_buf());
        }
        dir = dir.parent()?;
    }
}

/// Pi's `collectAncestorAgentsSkillDirs`: `.agents/skills` in `cwd` and
/// each parent up to the git repository root, or to the filesystem root
/// outside a repository, nearest first.
pub fn ancestor_agents_skill_dirs(cwd: &Path) -> Vec<PathBuf> {
    let start = resolve_path(cwd);
    let repo_root = find_git_repo_root(&start);
    let mut dirs = Vec::new();
    let mut dir = start.as_path();
    loop {
        dirs.push(dir.join(".agents").join("skills"));
        if repo_root.as_deref() == Some(dir) {
            break;
        }
        match dir.parent() {
            Some(parent) => dir = parent,
            None => break,
        }
    }
    dirs
}

/// The user's `~/.agents/skills`.
pub fn user_agents_skills_dir(user_home: &Path) -> PathBuf {
    user_home.join(".agents").join("skills")
}

/// Pi's `resolvePath(input, cwd, { trim: true })`.
fn resolve_against(input: &str, cwd: &Path) -> PathBuf {
    let normalized = normalize_path(trim(input));
    let joined = if normalized.is_absolute() {
        normalized
    } else {
        cwd.join(normalized)
    };
    lexical_normalize(&joined)
}

struct Loaded {
    result: LoadSkillsResult,
    real_paths: HashSet<PathBuf>,
    collisions: Vec<SkillDiagnostic>,
}

impl Loaded {
    /// Pi's `addSkills`: a file already loaded through another path is
    /// skipped silently, and a second skill of a name is a collision.
    fn add(&mut self, loaded: LoadSkillsResult) {
        self.result.diagnostics.extend(loaded.diagnostics);
        for skill in loaded.skills {
            let real = fs::canonicalize(&skill.file_path)
                .unwrap_or_else(|_| PathBuf::from(&skill.file_path));
            if self.real_paths.contains(&real) {
                continue;
            }
            match self
                .result
                .skills
                .iter()
                .find(|known| known.name == skill.name)
            {
                Some(existing) => self.collisions.push(SkillDiagnostic {
                    kind: SkillDiagnosticKind::Collision,
                    message: format!("name \"{}\" collision", skill.name),
                    path: skill.file_path.clone(),
                    collision: Some(SkillCollision {
                        name: skill.name.clone(),
                        winner_path: existing.file_path.clone(),
                        loser_path: skill.file_path.clone(),
                    }),
                }),
                None => {
                    self.real_paths.insert(real);
                    self.result.skills.push(skill);
                }
            }
        }
    }
}

/// Pi's `loadSkills` over Bake's roots (see the module documentation):
/// the skills, the first of each name winning, and every problem found,
/// collisions last.
pub fn load_skills(options: &LoadSkillsOptions) -> LoadSkillsResult {
    let cwd = resolve_path(Path::new(&options.cwd));
    let mut loaded = Loaded {
        result: LoadSkillsResult::default(),
        real_paths: HashSet::new(),
        collisions: Vec::new(),
    };
    if options.include_defaults {
        let user_dir = options
            .user_home
            .as_deref()
            .map(|home| resolve_path(&user_agents_skills_dir(home)));
        for dir in ancestor_agents_skill_dirs(&cwd) {
            if user_dir.as_ref() != Some(&dir) {
                loaded.add(load_files(
                    &collect_skill_files(&dir, Discovery::Agents),
                    "project",
                ));
            }
        }
        if let Some(dir) = &user_dir {
            loaded.add(load_files(
                &collect_skill_files(dir, Discovery::Agents),
                "user",
            ));
        }
    }
    for raw in &options.skill_paths {
        let path = resolve_against(raw, &cwd);
        if !path.exists() {
            loaded
                .result
                .diagnostics
                .push(SkillDiagnostic::warning("skill path does not exist", &path));
            continue;
        }
        match fs::metadata(&path) {
            Ok(metadata) if metadata.is_dir() => loaded.add(load_skills_from_dir(&path, "path")),
            Ok(metadata) if metadata.is_file() && path.to_string_lossy().ends_with(".md") => {
                let (skill, diagnostics) = load_skill_from_file(&path, "path");
                match skill {
                    Some(skill) => loaded.add(LoadSkillsResult {
                        skills: vec![skill],
                        diagnostics,
                    }),
                    None => loaded.result.diagnostics.extend(diagnostics),
                }
            }
            Ok(_) => loaded.result.diagnostics.push(SkillDiagnostic::warning(
                "skill path is not a markdown file",
                &path,
            )),
            Err(error) => loaded
                .result
                .diagnostics
                .push(SkillDiagnostic::warning(error.to_string(), &path)),
        }
    }
    let Loaded {
        mut result,
        collisions,
        ..
    } = loaded;
    result.diagnostics.extend(collisions);
    result
}

/// Pi's `_expandSkillCommand`: `/skill:name args` becomes the skill's
/// body in a `<skill>` block, followed by the arguments. Text that is not a
/// skill command, names no loaded skill, or whose skill file cannot be
/// read is returned unchanged.
pub fn expand_skill_command(text: &str, skills: &[Skill]) -> String {
    let Some(rest) = text.strip_prefix("/skill:") else {
        return text.to_owned();
    };
    let (name, args) = match rest.split_once(' ') {
        Some((name, args)) => (name, trim(args)),
        None => (rest, ""),
    };
    let Some(skill) = skills.iter().find(|skill| skill.name == name) else {
        return text.to_owned();
    };
    let Ok(Some(content)) = read_text_file(Path::new(&skill.file_path), MAX_SKILL_FILE_BYTES)
    else {
        return text.to_owned();
    };
    let body = strip_frontmatter(&content);
    let block = format!(
        "<skill name=\"{}\" location=\"{}\">\nReferences are relative to {}.\n\n{}\n</skill>",
        skill.name,
        skill.file_path,
        skill.base_dir,
        trim(&body)
    );
    if args.is_empty() {
        block
    } else {
        format!("{block}\n\n{args}")
    }
}
