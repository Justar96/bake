//! The skills list's budget ([`format_skills_for_prompt`]): whole within
//! it, the longest descriptions evened out at a word boundary over it, the
//! tail's descriptions left out past the floor, never a skill left out,
//! and the notice whenever anything is shortened.

use proptest::prelude::*;

use super::*;

fn skill(name: &str, description: &str) -> Skill {
    Skill {
        name: name.into(),
        description: description.into(),
        file_path: format!("/skills/{name}/SKILL.md"),
        base_dir: format!("/skills/{name}"),
        source: "test".into(),
        disable_model_invocation: false,
    }
}

fn tokens(text: &str) -> u64 {
    bake_ai::utils::estimate::estimate_text_tokens(text)
}

fn list(skills: &[Skill], budget: u64) -> String {
    format_skills_for_prompt(skills, SkillReader::Read, budget)
}

fn unescape(text: &str) -> String {
    text.replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&amp;", "&")
}

/// Each listed skill's name, description (`None` when left out), and
/// location, in order, unescaped.
fn entries(text: &str) -> Vec<(String, Option<String>, String)> {
    let between = |text: &str, open: &str, close: &str| -> Option<(String, usize)> {
        let start = text.find(open)? + open.len();
        let end = start + text[start..].find(close)?;
        Some((unescape(&text[start..end]), end + close.len()))
    };
    let mut found = Vec::new();
    let mut rest = text;
    while let Some(start) = rest.find("  <skill>\n") {
        let end = start + rest[start..].find("  </skill>").expect("closed");
        let entry = &rest[start..end];
        let (name, _) = between(entry, "<name>", "</name>").expect("name");
        let description = between(entry, "<description>", "</description>").map(|(text, _)| text);
        let (location, _) = between(entry, "<location>", "</location>").expect("location");
        found.push((name, description, location));
        rest = &rest[end + "  </skill>".len()..];
    }
    found
}

/// A description of `words` five-character words: one ends every six
/// characters.
fn words(prefix: char, words: usize) -> String {
    vec![format!("{prefix}{prefix}{prefix}{prefix}{prefix}"); words].join(" ")
}

// Within the budget the list is whole and has no notice; one token less and
// it is shortened and says so.
#[test]
fn a_list_that_fits_is_whole() {
    let skills = [skill("one", &words('a', 60)), skill("two", "Short.")];
    let whole = list(&skills, u64::MAX);
    assert!(!whole.contains(SHORTENED_NOTICE));
    assert_eq!(list(&skills, tokens(&whole)), whole);
    let shortened = list(&skills, tokens(&whole) - 1);
    assert_ne!(shortened, whole);
    assert!(shortened.starts_with(&format!(
        "\n\nThe following skills provide specialized instructions for specific tasks. {SHORTENED_NOTICE}\n"
    )));
}

// Over the budget the longest descriptions are cut to one length at a word
// boundary, ending in `…`; one at or below that length is kept whole.
#[test]
fn the_longest_descriptions_are_evened_out() {
    let skills = [
        skill("long", &words('a', 100)),
        skill("short", &words('c', 10)),
        skill("middle", &words('b', 50)),
    ];
    let whole = list(&skills, u64::MAX);
    let text = list(&skills, tokens(&whole) - 100);
    let listed = entries(&text);
    let long = listed[0].1.clone().expect("long");
    let middle = listed[2].1.clone().expect("middle");
    assert_eq!(listed[1].1.as_deref(), Some(skills[1].description.as_str()));
    assert!(long.ends_with('…') && middle.ends_with('…'), "{text}");
    assert_eq!(long.chars().count(), middle.chars().count(), "evened out");
    let kept = long.trim_end_matches('…');
    assert!(kept.chars().count() >= DESCRIPTION_FLOOR);
    assert!(skills[0].description.starts_with(kept));
    assert!(
        skills[0].description[kept.len()..].starts_with(' '),
        "a word boundary"
    );
    assert!(tokens(&text) <= tokens(&whole) - 100);
}

// A description no longer than the floor is never cut, and none is cut
// below it: past that, the last skills lose their description.
#[test]
fn past_the_floor_the_tail_loses_its_descriptions() {
    let skills: Vec<Skill> = (0..6)
        .map(|index| skill(&format!("skill-{index}"), &words('a', 60)))
        .collect();
    let floor = render_skills(
        &skills.iter().collect::<Vec<_>>(),
        SkillReader::Read,
        Cut::Cap(DESCRIPTION_FLOOR),
    );
    let text = list(&skills, tokens(&floor) - 60);
    let listed = entries(&text);
    assert_eq!(listed.len(), 6);
    let described = listed.iter().take_while(|entry| entry.1.is_some()).count();
    assert!((1..6).contains(&described), "{text}");
    assert!(listed[described..].iter().all(|entry| entry.1.is_none()));
    for (_, description, _) in &listed[..described] {
        let description = description.as_deref().expect("description");
        assert!(description.trim_end_matches('…').chars().count() >= DESCRIPTION_FLOOR);
    }
    assert!(text.contains(SHORTENED_NOTICE));
    // A description no longer than the floor is not cut, but can be left out.
    let short = [skill("a", &"x".repeat(DESCRIPTION_FLOOR)), skill("b", "y")];
    let text = list(&short, 1);
    assert_eq!(
        entries(&text),
        [
            ("a".into(), None, "/skills/a/SKILL.md".into()),
            ("b".into(), None, "/skills/b/SKILL.md".into())
        ],
        "over any budget, names and locations stay"
    );
}

// The exact text of a shortened entry and of the opening with the notice.
#[test]
fn a_shortened_entry_reads_so() {
    let description = "Run and review Bake's Cargo checks: formatting, Clippy, unit and integration tests, property tests, mutation testing, coverage, and parser fuzzing, with the commands and limits for each.";
    let skills = [
        skill("bake-testing", description),
        skill("other", &words('a', 40)),
    ];
    let whole = list(&skills, u64::MAX);
    let text = list(&skills, tokens(&whole) - 10);
    assert!(text.contains(
        "  <skill>\n    <name>bake-testing</name>\n    <description>Run and review Bake's Cargo checks: formatting, Clippy, unit and integration tests, property tests, mutation testing, coverage, and parser fuzzing, with…</description>\n    <location>/skills/bake-testing/SKILL.md</location>\n  </skill>"
    ), "{text}");
    assert!(text.starts_with(
        "\n\nThe following skills provide specialized instructions for specific tasks. Some descriptions below are shortened or left out; a skill's file holds the whole one.\nUse the read tool"
    ));
}

fn any_skill() -> impl Strategy<Value = Skill> {
    (
        "[a-z0-9-]{1,12}",
        "([a-zA-Z<>&'\"é漢🙂]{0,12}[ \n]{0,2}){0,40}",
        "/[a-z/<&]{0,16}",
    )
        .prop_map(|(name, description, dir)| Skill {
            file_path: format!("{dir}/SKILL.md"),
            base_dir: dir,
            name,
            description,
            source: "test".into(),
            disable_model_invocation: false,
        })
}

/// How much of a description a list keeps, for comparing two lists: none,
/// the characters before `…`, or all of it.
fn kept(original: &str, listed: Option<&str>) -> usize {
    match listed {
        None => 0,
        Some(listed) if listed == original => usize::MAX,
        Some(listed) => listed.trim_end_matches('…').chars().count(),
    }
}

proptest! {
    #[test]
    fn the_budgeted_list_holds_its_invariants(
        skills in proptest::collection::vec(any_skill(), 1..8),
        budget in 0_u64..2000,
        more in 0_u64..2000,
    ) {
        let text = list(&skills, budget);
        let refs: Vec<&Skill> = skills.iter().collect();
        let names_only = render_skills(&refs, SkillReader::Read, Cut::Drop(skills.len()));
        if tokens(&names_only) <= budget {
            prop_assert!(tokens(&text) <= budget);
        }
        let listed = entries(&text);
        prop_assert_eq!(listed.len(), skills.len());
        for ((name, description, location), skill) in listed.iter().zip(&skills) {
            prop_assert_eq!(name, &skill.name);
            prop_assert_eq!(location, &skill.file_path);
            if let Some(description) = description
                && description != &skill.description
            {
                let cut = description.strip_suffix('…');
                prop_assert!(cut.is_some_and(|cut| skill.description.starts_with(cut)));
            }
        }
        prop_assert_eq!(text.contains(SHORTENED_NOTICE), text != render_skills(&refs, SkillReader::Read, Cut::Whole));
        // Deterministic, and a larger budget never shortens more.
        prop_assert_eq!(&list(&skills, budget), &text);
        let larger = entries(&list(&skills, budget + more));
        for ((small, large), skill) in listed.iter().zip(&larger).zip(&skills) {
            prop_assert!(kept(&skill.description, large.1.as_deref()) >= kept(&skill.description, small.1.as_deref()));
        }
        prop_assert_eq!(list(&skills, u64::MAX), render_skills(&refs, SkillReader::Read, Cut::Whole));
    }

    #[test]
    fn shortening_never_splits_a_character(text in "\\PC{0,300}", cap in 0_usize..400) {
        let short = shorten(&text, cap.max(DESCRIPTION_FLOOR));
        let kept = short.strip_suffix('…').filter(|_| short != text).unwrap_or(&short);
        prop_assert!(text.starts_with(kept));
    }
}
