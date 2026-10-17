//! Ports Pi `test/skills.test.ts` (v1.2.0) over its fixtures, copied to
//! `tests/fixtures/pi-skills/`; the `.agents/skills` auto-discovery, ignore
//! file, and base-directory cases of `test/package-manager.test.ts` for the
//! roots Bake reads; and the skills cases of `test/system-prompt.test.ts`,
//! with the prompt checked against fixtures recorded from Pi
//! (`tests/fixtures/system_prompt/skills.generate.ts`).

use std::path::{Path, PathBuf};

use super::*;
use crate::system_prompt::{SystemPromptOptions, build_system_prompt_sections};
use crate::test_support::{TempDir, write};

fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/pi-skills/skills")
}

fn collision_fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/pi-skills/skills-collision")
}

fn from_fixture(name: &str) -> LoadSkillsResult {
    load_skills_from_dir(&fixtures().join(name), "test")
}

fn mentions(result: &LoadSkillsResult, text: &str) -> bool {
    result
        .diagnostics
        .iter()
        .any(|diagnostic| diagnostic.message.contains(text))
}

fn skill(name: &str, description: &str, dir: &str) -> Skill {
    Skill {
        name: name.into(),
        description: description.into(),
        file_path: format!("{dir}/SKILL.md"),
        base_dir: dir.into(),
        source: "test".into(),
        disable_model_invocation: false,
    }
}

fn skill_file(name: &str, description: &str) -> String {
    format!("---\nname: {name}\ndescription: {description}\n---\n")
}

/// A temporary directory that is its own repository root, so the ancestor
/// `.agents/skills` walk stops inside it. Windows keeps its temporary
/// directory under the user's profile, whose real `~/.agents/skills` the
/// walk would otherwise read.
fn bounded_temp(name: &str) -> TempDir {
    let temp = TempDir::new(name);
    std::fs::create_dir_all(temp.join(".git")).expect(".git");
    temp
}

fn defaults(cwd: &Path, user_home: Option<&Path>) -> LoadSkillsOptions {
    LoadSkillsOptions {
        cwd: cwd.to_string_lossy().into_owned(),
        user_home: user_home.map(Path::to_path_buf),
        skill_paths: Vec::new(),
        include_defaults: true,
    }
}

fn paths(result: &LoadSkillsResult) -> Vec<String> {
    result
        .skills
        .iter()
        .map(|skill| skill.file_path.clone())
        .collect()
}

/// A path as the loader reports it. The loader joins one component at a
/// time, so on Windows its paths use `\` throughout, while the tests join
/// literals such as `.agents/skills/x/SKILL.md`.
fn path(path: PathBuf) -> String {
    let path = path.to_string_lossy().into_owned();
    if cfg!(windows) {
        path.replace('/', "\\")
    } else {
        path
    }
}

// describe("loadSkillsFromDir")

#[test]
fn loads_a_valid_skill() {
    let result = from_fixture("valid-skill");
    assert_eq!(result.skills.len(), 1);
    assert_eq!(result.skills[0].name, "valid-skill");
    assert_eq!(
        result.skills[0].description,
        "A valid skill for testing purposes."
    );
    assert_eq!(result.skills[0].source, "test");
    assert!(!result.skills[0].disable_model_invocation);
    assert_eq!(result.diagnostics, []);
}

#[test]
fn allows_names_that_do_not_match_the_parent_directory() {
    let result = from_fixture("name-mismatch");
    assert_eq!(result.skills.len(), 1);
    assert_eq!(result.skills[0].name, "different-name");
    assert!(!mentions(&result, "does not match parent directory"));
}

#[test]
fn warns_on_invalid_names_but_loads() {
    let result = from_fixture("invalid-name-chars");
    assert_eq!(result.skills.len(), 1);
    assert!(mentions(&result, "invalid characters"));

    let result = from_fixture("long-name");
    assert_eq!(result.skills.len(), 1);
    assert!(mentions(&result, "exceeds 64 characters"));

    let result = from_fixture("consecutive-hyphens");
    assert_eq!(result.skills.len(), 1);
    assert!(mentions(&result, "consecutive hyphens"));
}

#[test]
fn skips_a_skill_without_a_description() {
    let result = from_fixture("missing-description");
    assert_eq!(result.skills, []);
    assert!(mentions(&result, "description is required"));

    // "should skip files without frontmatter"
    let result = from_fixture("no-frontmatter");
    assert_eq!(result.skills, []);
    assert!(mentions(&result, "description is required"));
}

#[test]
fn ignores_unknown_frontmatter_fields() {
    let result = from_fixture("unknown-field");
    assert_eq!(result.skills.len(), 1);
    assert_eq!(result.diagnostics, []);
}

#[test]
fn loads_nested_skills_and_prefers_a_root_skill_file() {
    let result = from_fixture("nested");
    assert_eq!(result.skills.len(), 1);
    assert_eq!(result.skills[0].name, "child-skill");
    assert_eq!(result.diagnostics, []);

    let result = from_fixture("root-skill-preferred");
    assert_eq!(result.skills.len(), 1);
    assert_eq!(result.skills[0].name, "root-skill-preferred");
    assert_eq!(result.skills[0].description, "Root skill should win.");
    assert_eq!(result.diagnostics, []);
}

#[test]
fn invalid_yaml_is_a_warning_with_its_position() {
    let result = from_fixture("invalid-yaml");
    assert_eq!(result.skills, []);
    assert!(mentions(&result, "at line"), "{:?}", result.diagnostics);
}

#[test]
fn keeps_multiline_descriptions() {
    let result = from_fixture("multiline-description");
    assert_eq!(result.skills.len(), 1);
    assert!(result.skills[0].description.contains('\n'));
    assert!(
        result.skills[0]
            .description
            .contains("This is a multiline description.")
    );
    assert_eq!(result.diagnostics, []);
}

#[test]
fn loads_every_fixture_with_a_description() {
    let result = load_skills_from_dir(&fixtures(), "test");
    assert!(result.skills.len() >= 6);
    // Node lists directory entries sorted on Unix; so does the port.
    let names: Vec<&str> = result
        .skills
        .iter()
        .map(|skill| skill.name.as_str())
        .collect();
    assert_eq!(
        names,
        [
            "bad--name",
            "disable-model-invocation",
            "Invalid_Name",
            "this-is-a-very-long-skill-name-that-exceeds-the-sixty-four-character-limit-set-by-the-standard",
            "multiline-description",
            "different-name",
            "child-skill",
            "root-skill-preferred",
            "unknown-field",
            "valid-skill",
        ]
    );
}

#[test]
fn a_missing_directory_is_empty() {
    let result = load_skills_from_dir(Path::new("/non/existent/path"), "test");
    assert_eq!(result, LoadSkillsResult::default());
}

#[test]
fn parses_disable_model_invocation() {
    let result = from_fixture("disable-model-invocation");
    assert_eq!(result.skills.len(), 1);
    assert!(result.skills[0].disable_model_invocation);
    assert!(!mentions(&result, "unknown frontmatter field"));
}

// describe("formatSkillsForPrompt")

#[test]
fn formats_skills_as_xml() {
    assert_eq!(format_skills_for_prompt(&[], SkillReader::Read), "");
    let one = [skill("test-skill", "A test skill.", "/path/to/skill")];
    let text = format_skills_for_prompt(&one, SkillReader::Read);
    for part in [
        "<available_skills>",
        "</available_skills>",
        "<skill>",
        "<name>test-skill</name>",
        "<description>A test skill.</description>",
        "<location>/path/to/skill/SKILL.md</location>",
    ] {
        assert!(text.contains(part), "{part}");
    }
    let intro = &text[..text.find("<available_skills>").unwrap_or(0)];
    assert!(intro.contains("The following skills provide specialized instructions"));
    assert!(intro.contains("Use the read tool to load a skill's file"));

    let escaped = [skill(
        "test-skill",
        "A skill with <special> & \"characters\".",
        "/path/to/skill",
    )];
    let text = format_skills_for_prompt(&escaped, SkillReader::Read);
    assert!(text.contains("&lt;special&gt;"));
    assert!(text.contains("&amp;"));
    assert!(text.contains("&quot;characters&quot;"));

    let two = [
        skill("skill-one", "First skill.", "/path/one"),
        skill("skill-two", "Second skill.", "/path/two"),
    ];
    let text = format_skills_for_prompt(&two, SkillReader::Read);
    assert!(text.contains("<name>skill-one</name>"));
    assert!(text.contains("<name>skill-two</name>"));
    assert_eq!(text.matches("<skill>").count(), 2);
}

#[test]
fn hidden_skills_stay_out_of_the_prompt() {
    let mut hidden = skill("hidden-skill", "A hidden skill.", "/path/hidden");
    hidden.disable_model_invocation = true;
    let both = [
        skill("visible-skill", "A visible skill.", "/path/visible"),
        hidden.clone(),
    ];
    let text = format_skills_for_prompt(&both, SkillReader::Read);
    assert!(text.contains("<name>visible-skill</name>"));
    assert!(!text.contains("<name>hidden-skill</name>"));
    assert_eq!(text.matches("<skill>").count(), 1);
    assert_eq!(format_skills_for_prompt(&[hidden], SkillReader::Read), "");
}

// describe("loadSkills with options")

#[test]
fn loads_explicit_paths() {
    let temp = bounded_temp("skills-explicit");
    let cwd = temp.join("cwd");
    std::fs::create_dir_all(&cwd).expect("cwd");
    let options = |paths: &[String]| LoadSkillsOptions {
        skill_paths: paths.to_vec(),
        ..defaults(&cwd, Some(&temp.join("home")))
    };

    let result = load_skills(&options(&[path(fixtures().join("valid-skill"))]));
    assert_eq!(result.skills.len(), 1);
    assert_eq!(result.skills[0].source, "path");
    assert_eq!(result.diagnostics, []);

    let result = load_skills(&options(&["/non/existent/path".into()]));
    assert_eq!(result.skills, []);
    assert!(mentions(&result, "does not exist"));

    // A file path, relative to the working directory and trimmed.
    write(
        &cwd.join("one.md"),
        &skill_file("one", "A skill in one file."),
    );
    write(&cwd.join("notes.txt"), "not markdown");
    let result = load_skills(&options(&["  one.md ".into(), "notes.txt".into()]));
    assert_eq!(paths(&result), [path(cwd.join("one.md"))]);
    assert_eq!(result.skills[0].base_dir, path(cwd.clone()));
    assert!(mentions(&result, "skill path is not a markdown file"));
}

// describe("collision handling")

#[test]
fn the_first_skill_of_a_name_wins() {
    let temp = bounded_temp("skills-collision");
    let cwd = temp.join("cwd");
    std::fs::create_dir_all(&cwd).expect("cwd");
    let first = collision_fixtures().join("first");
    let second = collision_fixtures().join("second");
    let result = load_skills(&LoadSkillsOptions {
        skill_paths: vec![path(first.clone()), path(second.clone())],
        ..defaults(&cwd, None)
    });
    assert_eq!(result.skills.len(), 1);
    assert_eq!(result.skills[0].description, "First calendar skill.");
    let collisions: Vec<&SkillDiagnostic> = result
        .diagnostics
        .iter()
        .filter(|diagnostic| diagnostic.kind == SkillDiagnosticKind::Collision)
        .collect();
    assert_eq!(collisions.len(), 1);
    assert_eq!(collisions[0].message, "name \"calendar\" collision");
    assert_eq!(
        collisions[0].collision,
        Some(SkillCollision {
            name: "calendar".into(),
            winner_path: path(first.join("calendar/SKILL.md")),
            loser_path: path(second.join("calendar/SKILL.md")),
        })
    );
}

// Pi test/package-manager.test.ts: describe(".agents/skills
// auto-discovery") and describe("auto-discovered skill metadata").

#[test]
fn scans_agents_skills_up_to_the_git_root() {
    let temp = TempDir::new("skills-git-root");
    let repo = temp.join("repo");
    let cwd = repo.join("packages").join("feature");
    std::fs::create_dir_all(&cwd).expect("cwd");
    std::fs::create_dir_all(repo.join(".git")).expect(".git");
    let above = temp.join(".agents/skills/above-repo/SKILL.md");
    let root = repo.join(".agents/skills/repo-root/SKILL.md");
    let nested = repo.join("packages/.agents/skills/nested/SKILL.md");
    write(&above, &skill_file("above-repo", "above"));
    write(&root, &skill_file("repo-root", "repo"));
    write(&nested, &skill_file("nested", "nested"));

    let result = load_skills(&defaults(&cwd, None));
    // Nearest first, so a nested skill outranks the repository's.
    assert_eq!(paths(&result), [path(nested.clone()), path(root.clone())]);
    assert!(result.skills.iter().all(|skill| skill.source == "project"));
    assert_eq!(
        result.skills[0].base_dir,
        path(repo.join("packages/.agents/skills/nested"))
    );
}

#[test]
fn scans_to_the_filesystem_root_outside_a_repository() {
    let temp = TempDir::new("skills-no-repo");
    let root = temp.join("non-repo");
    let cwd = root.join("a").join("b");
    std::fs::create_dir_all(&cwd).expect("cwd");
    let top = root.join(".agents/skills/root/SKILL.md");
    let middle = root.join("a/.agents/skills/middle/SKILL.md");
    write(&top, &skill_file("root", "root"));
    write(&middle, &skill_file("middle", "middle"));

    let result = load_skills(&defaults(&cwd, None));
    let found = paths(&result);
    assert!(found.contains(&path(top)));
    assert!(found.contains(&path(middle)));
}

#[test]
fn agents_roots_skip_root_markdown_and_keep_nested_markdown() {
    let temp = bounded_temp("skills-agents-markdown");
    let skills = temp.join(".agents/skills");
    let root_file = skills.join("root-file.md");
    let nested_skill = skills.join("nested-skill/SKILL.md");
    let nested_markdown = skills.join("third-party/child-skill.md");
    let deep_markdown = skills.join("third-party/vendor/pack/deep-skill.md");
    let undescribed = skills.join("third-party/README.md");
    write(&root_file, &skill_file("root-file", "Root markdown file"));
    write(&nested_skill, &skill_file("nested-skill", "Nested skill"));
    write(
        &nested_markdown,
        &skill_file("child-skill", "Nested markdown skill"),
    );
    write(
        &deep_markdown,
        &skill_file("deep-skill", "Deep markdown skill"),
    );
    write(&undescribed, "# Third-party skills\n");
    let cwd = temp.join("work");
    std::fs::create_dir_all(&cwd).expect("cwd");

    let result = load_skills(&defaults(&cwd, None));
    assert_eq!(
        paths(&result),
        [
            path(nested_skill),
            path(nested_markdown),
            path(deep_markdown)
        ]
    );
    // A Markdown file without a description is no skill and no problem.
    assert_eq!(result.diagnostics, []);
}

#[test]
fn the_users_agents_skills_load_once_as_user_skills() {
    let temp = bounded_temp("skills-user");
    let home = temp.join("home");
    let cwd = home.join("scratch").join("nested");
    std::fs::create_dir_all(&cwd).expect("cwd");
    let skill = home.join(".agents/skills/home-skill/SKILL.md");
    write(&skill, &skill_file("home-skill", "home"));

    let result = load_skills(&defaults(&cwd, Some(&home)));
    assert_eq!(paths(&result), [path(skill)]);
    assert_eq!(result.skills[0].source, "user");
    assert_eq!(
        result.skills[0].base_dir,
        path(home.join(".agents/skills/home-skill"))
    );

    // `--no-skills` reads no root.
    let result = load_skills(&LoadSkillsOptions {
        include_defaults: false,
        ..defaults(&cwd, Some(&home))
    });
    assert_eq!(result, LoadSkillsResult::default());
}

#[test]
fn project_skills_win_over_user_skills_of_the_same_name() {
    let temp = TempDir::new("skills-precedence");
    let home = temp.join("home");
    let cwd = temp.join("project");
    std::fs::create_dir_all(cwd.join(".git")).expect(".git");
    let project = cwd.join(".agents/skills/review/SKILL.md");
    let user = home.join(".agents/skills/review/SKILL.md");
    write(&project, &skill_file("review", "Project review"));
    write(&user, &skill_file("review", "User review"));

    let result = load_skills(&defaults(&cwd, Some(&home)));
    assert_eq!(paths(&result), [path(project)]);
    assert_eq!(result.diagnostics.len(), 1);
    assert_eq!(result.diagnostics[0].kind, SkillDiagnosticKind::Collision);
    assert_eq!(result.diagnostics[0].path, path(user));
}

#[cfg(unix)]
#[test]
fn a_file_reached_through_a_link_loads_once() {
    let temp = TempDir::new("skills-link-dedupe");
    let home = temp.join("home");
    let cwd = temp.join("project");
    std::fs::create_dir_all(cwd.join(".git")).expect(".git");
    let skill = home.join(".agents/skills/foo/SKILL.md");
    write(&skill, &skill_file("foo", "foo"));
    std::fs::create_dir_all(cwd.join(".agents")).expect(".agents");
    std::os::unix::fs::symlink(home.join(".agents/skills"), cwd.join(".agents/skills"))
        .expect("link");

    let result = load_skills(&defaults(&cwd, Some(&home)));
    assert_eq!(result.skills.len(), 1);
    assert_eq!(result.diagnostics, []);
}

#[cfg(unix)]
#[test]
fn link_cycles_end() {
    let temp = bounded_temp("skills-link-cycle");
    let skills = temp.join(".agents/skills");
    write(&skills.join("a/b/SKILL.md"), &skill_file("b", "b"));
    std::fs::create_dir_all(skills.join("loop")).expect("loop");
    // Two links back to the root: Pi would revisit the tree exponentially.
    std::os::unix::fs::symlink(&skills, skills.join("loop/one")).expect("link");
    std::os::unix::fs::symlink(&skills, skills.join("loop/two")).expect("link");
    std::os::unix::fs::symlink(skills.join("missing"), skills.join("dangling")).expect("link");
    let cwd = temp.join("work");
    std::fs::create_dir_all(&cwd).expect("cwd");

    let result = load_skills(&defaults(&cwd, None));
    assert_eq!(paths(&result), [path(skills.join("a/b/SKILL.md"))]);
}

// describe("ignore files")

#[test]
fn ignore_files_apply_below_their_directory() {
    let temp = bounded_temp("skills-ignore");
    let skills = temp.join(".agents/skills");
    write(&skills.join(".gitignore"), "venv\n__pycache__\n");
    write(
        &skills.join("good-skill/SKILL.md"),
        &skill_file("good-skill", "Good"),
    );
    write(
        &skills.join("venv/bad-skill/SKILL.md"),
        &skill_file("bad-skill", "Bad"),
    );
    // A nested ignore file is scoped to its directory, and `!` re-includes.
    write(
        &skills.join("pack/.ignore"),
        "*.md\n!keep.md\n# comment\n\\#literal.md\n",
    );
    write(&skills.join("pack/drop.md"), &skill_file("drop", "Drop"));
    write(&skills.join("pack/keep.md"), &skill_file("keep", "Keep"));
    write(
        &skills.join("pack/#literal.md"),
        &skill_file("literal", "Literal"),
    );
    write(
        &skills.join("other/drop.md"),
        &skill_file("other-drop", "Other"),
    );
    let cwd = temp.join("work");
    std::fs::create_dir_all(&cwd).expect("cwd");

    let result = load_skills(&defaults(&cwd, None));
    let names: Vec<&str> = result
        .skills
        .iter()
        .map(|skill| skill.name.as_str())
        .collect();
    assert_eq!(names, ["good-skill", "other-drop", "keep"]);
}

#[test]
fn a_parent_ignore_file_does_not_hide_a_root() {
    // "should not apply parent .gitignore to .pi auto-discovery", for
    // `.agents`.
    let temp = bounded_temp("skills-parent-ignore");
    write(&temp.join(".gitignore"), ".agents\n");
    let skill = temp.join(".agents/skills/auto-skill/SKILL.md");
    write(&skill, &skill_file("auto-skill", "Auto"));
    let cwd = temp.join("work");
    std::fs::create_dir_all(&cwd).expect("cwd");

    let result = load_skills(&defaults(&cwd, None));
    assert_eq!(paths(&result), [path(skill)]);
}

// Frontmatter, Pi `src/utils/frontmatter.ts`.

#[test]
fn frontmatter_follows_pi() {
    let (front, body) =
        parse_frontmatter("\u{feff}---\r\nname: x\r\n---\r\n\r\n Body \r\n").unwrap_or_default();
    assert_eq!(front["name"], "x");
    assert_eq!(body, "Body");
    let (front, body) = parse_frontmatter("# Title\n").unwrap_or_default();
    assert_eq!(front, serde_json::json!({}));
    assert_eq!(body, "# Title\n");
    // An unclosed block is no frontmatter.
    let (front, _) = parse_frontmatter("---\nname: x\n").unwrap_or_default();
    assert_eq!(front, serde_json::json!({}));
    // An empty block is an empty object.
    let (front, body) = parse_frontmatter("---\n---\nBody").unwrap_or_default();
    assert_eq!(front, serde_json::json!({}));
    assert_eq!(body, "Body");
    assert_eq!(strip_frontmatter("---\na: 1\n---\n\nText\n"), "Text");
}

#[test]
fn name_and_description_validation() {
    assert_eq!(validate_name("ok-name-1"), Vec::<String>::new());
    assert_eq!(
        validate_name("-bad-"),
        ["name must not start or end with a hyphen"]
    );
    assert_eq!(
        validate_name(""),
        ["name contains invalid characters (must be lowercase a-z, 0-9, hyphens only)"]
    );
    assert_eq!(
        validate_description(Some(&"x".repeat(1025))),
        ["description exceeds 1024 characters (1025)"]
    );
    assert_eq!(
        validate_description(Some("  ")),
        ["description is required"]
    );
    assert_eq!(validate_description(None), ["description is required"]);
}

#[test]
fn a_non_string_name_falls_back_to_the_directory() {
    let temp = TempDir::new("skills-non-string");
    let dir = temp.join("dir-name");
    write(
        &dir.join("SKILL.md"),
        "---\nname: 123\ndescription: Numbered\n---\n",
    );
    let result = load_skills_from_dir(&dir, "test");
    assert_eq!(result.skills[0].name, "dir-name");
}

// `/skill:name`, Pi's `_expandSkillCommand`.

#[test]
fn skill_commands_expand_to_the_skill_body() {
    let temp = TempDir::new("skills-expand");
    let dir = temp.join("review");
    write(
        &dir.join("SKILL.md"),
        "---\nname: review\ndescription: Review code\n---\n\n# Review\n\nSee ./checklist.md.\n",
    );
    let skills = load_skills_from_dir(&dir, "test").skills;
    let file = path(dir.join("SKILL.md"));
    let base = path(dir.clone());
    let block = format!(
        "<skill name=\"review\" location=\"{file}\">\nReferences are relative to {base}.\n\n# Review\n\nSee ./checklist.md.\n</skill>"
    );
    assert_eq!(expand_skill_command("/skill:review", &skills), block);
    assert_eq!(
        expand_skill_command("/skill:review  the parser  ", &skills),
        format!("{block}\n\nthe parser")
    );
    for unchanged in ["/skill:missing args", "review", "/skills:review"] {
        assert_eq!(expand_skill_command(unchanged, &skills), unchanged);
    }
    std::fs::remove_file(dir.join("SKILL.md")).expect("remove");
    assert_eq!(
        expand_skill_command("/skill:review", &skills),
        "/skill:review"
    );
}

// Pi test/system-prompt.test.ts: describe("skills") and the hidden-tool
// skill case. Bake's prompt is its own (D33), so the `skills` section, and
// whether there is one, is checked against the prompts recorded from Pi.

/// The `name` section of a whole prompt Pi recorded, with its tags.
fn recorded_section(case: &str, name: &str) -> Option<String> {
    let recorded: serde_json::Value = serde_json::from_str(include_str!(
        "../../tests/fixtures/system_prompt/skills.json"
    ))
    .unwrap_or_default();
    let prompt = recorded[case].as_str().unwrap_or_default();
    let (_, rest) = prompt.split_once(&format!("<{name}>\n"))?;
    let (section, _) = rest.split_once(&format!("\n</{name}>"))?;
    Some(format!("<{name}>\n{section}\n</{name}>"))
}

fn prompt_with_skills(tools: &[&str]) -> SystemPromptOptions {
    let mut options = SystemPromptOptions::new("/work/project");
    options.selected_tools = tools.iter().map(|tool| (*tool).to_owned()).collect();
    options.append_system_prompt = "Appended.".into();
    options.context_files = vec![crate::system_prompt::ContextFile {
        path: "/work/project/AGENTS.md".into(),
        content: "Rules.".into(),
    }];
    let mut hidden = skill("hidden", "Hidden.", "/home/u/.agents/skills/hidden");
    hidden.disable_model_invocation = true;
    options.skills = vec![
        skill(
            "alpha",
            "Uses <tags> & \"quotes\" and 'apostrophes'.",
            "/home/u/.agents/skills/alpha",
        ),
        hidden,
        skill(
            "beta",
            "Line one.\nLine two.",
            "/work/project/.agents/skills/beta",
        ),
    ];
    options
}

/// The built prompt's section names and its `skills` section.
fn built(options: &SystemPromptOptions) -> (Vec<String>, Option<String>) {
    let sections = build_system_prompt_sections(options).unwrap_or_default();
    let skills = sections
        .iter()
        .find(|(name, _)| name == "skills")
        .map(|(_, text)| text.clone());
    (sections.into_iter().map(|(name, _)| name).collect(), skills)
}

#[test]
fn the_skills_section_matches_pi() {
    let mut read = prompt_with_skills(&["read", "bash"]);
    read.docs_command = "bake-rs docs".into();
    read.permissions = "File sandbox: read-only.".into();
    let (names, skills) = built(&read);
    assert_eq!(skills, recorded_section("read", "skills"));
    // Like the context files, the list changes only between runs, so it
    // sits before the run's permissions and environment.
    assert_eq!(
        names,
        [
            "preamble",
            "rules",
            "docs",
            "run",
            "addendum",
            "project_context",
            "skills",
            "permissions",
            "environment"
        ]
    );

    let (_, skills) = built(&prompt_with_skills(&["bash"]));
    assert_eq!(skills, recorded_section("bash", "skills"));
    assert!(skills.is_some_and(|text| text.contains("Use bash to load a skill's file")));

    let mut indirect = prompt_with_skills(&["read", "bash", "run"]);
    indirect.hidden_tools = vec!["read".into(), "bash".into()];
    let (_, skills) = built(&indirect);
    assert_eq!(skills, recorded_section("indirect", "skills"));
    assert!(skills.is_some_and(|text| {
        text.contains("\nLoad a skill's file when the task matches its description.")
    }));

    // "omits skills without read or bash"
    let (_, skills) = built(&prompt_with_skills(&["write"]));
    assert_eq!(recorded_section("none", "skills"), None);
    assert_eq!(skills, None);

    let mut custom = prompt_with_skills(&["bash"]);
    custom.custom_prompt = Some("Custom.".into());
    let (_, skills) = built(&custom);
    assert_eq!(skills, recorded_section("custom", "skills"));
    assert!(skills.is_some());

    let mut all_hidden = prompt_with_skills(&["read"]);
    all_hidden
        .skills
        .retain(|skill| skill.disable_model_invocation);
    let (_, skills) = built(&all_hidden);
    assert_eq!(recorded_section("allHidden", "skills"), None);
    assert_eq!(skills, None);
}
