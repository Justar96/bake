//! `/changelog`, `/share`, and `/bug` through the driver, against a real
//! session saved in a directory this test alone owns, with the faux
//! provider and a fake `gh` script in place of the GitHub CLI, so nothing
//! reaches the network. The script records each call and copies what it
//! is asked to upload, so a test sees exactly what would have left the
//! machine, and that nothing does when the person declines.
//!
//! Follows Pi v1.3.0 `session-share.ts` (the gist path, its `gh` checks and
//! errors, and `test/session-share.test.ts`'s `gh gist create
//! --public=false <file>` call) and `bug-report.ts` (the description,
//! transcript, and delivery prompts of
//! `test/interactive-mode-bug-report-hint.test.ts`, the zip fallback, and
//! `recordInSession`), with Bake's confirmation before a share and its
//! GitHub issue in place of Pi's upload.

#![cfg(unix)]

use std::path::{Path, PathBuf};

use bake_coding_agent::bug_report::BAKE_REPOSITORY;
use bake_tui_view::copy;
use bake_tui_view::dialog::{DialogAnswer, DialogKind, DialogRequest, SecretText};

use super::*;

/// A directory this test alone owns: the project, the sessions, the Bake
/// home, and the fake `gh` with what it recorded.
struct Place(PathBuf);

impl Place {
    fn new(name: &str) -> Self {
        let dir = std::env::temp_dir().join(format!(
            "bake-tui-share-{name}-{}-{}",
            std::process::id(),
            bake_ai::now_ms()
        ));
        for sub in ["project", "sessions", "home", "gh"] {
            std::fs::create_dir_all(dir.join(sub)).expect("a fresh place");
        }
        let script = dir.join("gh").join("gh");
        std::fs::write(
            &script,
            r#"#!/bin/sh
dir=$(dirname "$0")
printf '%s\n' "$*" >> "$dir/calls"
case "$1 $2" in
  "auth status")
    if [ -f "$dir/logged-out" ]; then echo "not logged in" >&2; exit 1; fi
    exit 0 ;;
  "gist create")
    cp "$4" "$dir/uploaded.html"
    echo "https://gist.github.com/someone/abc123"
    exit 0 ;;
  "issue create")
    shift 2
    while [ $# -gt 0 ]; do
      case "$1" in
        --body-file) cp "$2" "$dir/body.md" ;;
        --title) printf '%s' "$2" > "$dir/title" ;;
        --repo) printf '%s' "$2" > "$dir/repo" ;;
      esac
      shift
    done
    if [ -f "$dir/issue-fails" ]; then echo "HTTP 403: Resource not accessible" >&2; exit 1; fi
    echo "Creating issue in Justar96/bake"
    echo "https://github.com/Justar96/bake/issues/42"
    exit 0 ;;
esac
exit 2
"#,
        )
        .expect("script");
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).expect("mode");
        Self(dir)
    }

    fn gh(&self) -> PathBuf {
        self.0.join("gh").join("gh")
    }

    fn project(&self) -> PathBuf {
        self.0.join("project")
    }

    fn recorded(&self, name: &str) -> Option<String> {
        std::fs::read_to_string(self.0.join("gh").join(name)).ok()
    }

    fn calls(&self) -> Vec<String> {
        self.recorded("calls")
            .unwrap_or_default()
            .lines()
            .map(str::to_owned)
            .collect()
    }

    fn mark(&self, name: &str) {
        std::fs::write(self.0.join("gh").join(name), "").expect("mark");
    }

    /// Every line of the session file.
    fn session_lines(&self) -> Vec<serde_json::Value> {
        let mut found = Vec::new();
        let mut dirs = vec![self.0.join("sessions")];
        while let Some(dir) = dirs.pop() {
            for entry in std::fs::read_dir(dir).expect("sessions").flatten() {
                let path = entry.path();
                if path.is_dir() {
                    dirs.push(path);
                } else if path.extension().is_some_and(|ext| ext == "jsonl") {
                    let text = std::fs::read_to_string(&path).expect("session");
                    found.extend(
                        text.lines()
                            .map(|line| serde_json::from_str(line).expect("json line")),
                    );
                }
            }
        }
        found
    }
}

impl Drop for Place {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// A started port over a session saved under `place` whose Bake home has
/// a `settings.json` with a key in it, running `gh` as `program`.
fn share_harness(place: &Place, program: PathBuf) -> Harness {
    let manager = SessionManager::create(
        &place.project().to_string_lossy(),
        Some(&place.0.join("sessions")),
        NewSessionOptions::default(),
    )
    .expect("session");
    std::fs::write(
        place.0.join("home").join("settings.json"),
        r#"{"cliproxyapi":{"apiKey":"sk-settings-secret"},"theme":"dark"}"#,
    )
    .expect("settings");
    let (runtime, faux, mut live) = setup_with(manager, Vec::new(), Vec::new(), Some("test-key"));
    live.home = Some(place.0.join("home"));
    live.gh = Some(program.into_os_string());
    let (sender, updates) = std_mpsc::channel();
    let mut port = LivePort::start(live, move |update| sender.send(update).is_ok());
    port.begin();
    Harness {
        runtime,
        faux,
        port: Some(port),
        updates,
    }
}

impl Harness {
    /// The next dialog the runtime opens.
    fn sheet(&self) -> DialogRequest {
        let seen = self.until(|update| matches!(update, RuntimeUpdate::Dialog(Some(_))));
        match seen.last() {
            Some(RuntimeUpdate::Dialog(Some(request))) => request.clone(),
            _ => unreachable!(),
        }
    }

    fn reply_to(&self, dialog: &DialogRequest, answer: DialogAnswer) {
        self.port().request(Request::Dialog {
            id: dialog.id,
            answer,
        });
    }

    fn choose(&self, dialog: &DialogRequest, value: &str) {
        self.reply_to(
            dialog,
            DialogAnswer::Chose {
                value: value.into(),
                save_default: false,
            },
        );
    }

    /// Updates until prompt `id` is admitted, with the note and status
    /// that follow, or refused.
    fn settled(&self, id: u64) -> Vec<RuntimeUpdate> {
        let mut seen = self.until(|update| {
            matches!(update, RuntimeUpdate::Admitted(admitted) if *admitted == id)
                || matches!(update, RuntimeUpdate::Refused { id: refused, .. } if *refused == id)
        });
        if matches!(seen.last(), Some(RuntimeUpdate::Admitted(_))) {
            seen.extend(self.until(|update| matches!(update, RuntimeUpdate::Status(_))));
        }
        seen
    }

    /// One answered turn, so the session file exists.
    fn talk(&self, id: u64, text: &str, reply: &str) {
        self.faux
            .set_responses(vec![faux_assistant_message(reply).into()]);
        self.submit_id(id, Submission::FollowUp(text.into()));
        self.until_turn_end();
    }
}

fn admitted(updates: &[RuntimeUpdate], id: u64) -> bool {
    updates
        .iter()
        .any(|update| matches!(update, RuntimeUpdate::Admitted(admitted) if *admitted == id))
}

fn refused(updates: &[RuntimeUpdate]) -> Option<String> {
    updates.iter().find_map(|update| match update {
        RuntimeUpdate::Refused { reason, .. } => Some(reason.clone()),
        _ => None,
    })
}

fn note(updates: &[RuntimeUpdate]) -> Option<String> {
    committed(updates).into_iter().find_map(|row| match row {
        Row::Info(text) => Some(text),
        _ => None,
    })
}

fn list_note(dialog: &DialogRequest) -> String {
    match &dialog.kind {
        DialogKind::List(list) => list.note.clone().unwrap_or_default(),
        other => panic!("a list, not {other:?}"),
    }
}

fn decoded_session(html: &str) -> String {
    use base64::Engine;
    let marker = r#"<script id="session-data" type="application/json">"#;
    let start = html.find(marker).expect("data") + marker.len();
    let end = start + html[start..].find("</script>").expect("end");
    String::from_utf8(
        base64::engine::general_purpose::STANDARD
            .decode(&html[start..end])
            .expect("base64"),
    )
    .expect("utf-8")
}

// Pi `shareSession`'s gist path after Bake's confirmation: `gh auth
// status`, then `gh gist create --public=false <file>` with the HTML
// page, from a temporary directory that is gone afterwards.
#[test]
fn share_uploads_the_page_only_after_yes() {
    let place = Place::new("yes");
    let h = share_harness(&place, place.gh());
    h.talk(1, "hello there", "general kenobi");
    h.submit_id(2, Submission::FollowUp("/share".into()));
    let dialog = h.sheet();
    assert_eq!(list_note(&dialog), copy::SHARE_CONFIRM_NOTE);
    let DialogKind::List(list) = &dialog.kind else {
        unreachable!()
    };
    assert_eq!(list.selected, Some(1), "No is selected first");
    // Only the login check has run while the sheet asks.
    assert_eq!(place.calls(), ["auth status"]);
    assert!(place.recorded("uploaded.html").is_none());
    h.choose(&dialog, "yes");
    let updates = h.settled(2);
    assert!(admitted(&updates, 2));
    assert_eq!(
        note(&updates).as_deref(),
        Some("Gist: https://gist.github.com/someone/abc123")
    );
    let calls = place.calls();
    assert_eq!(calls.len(), 2);
    let upload = calls[1]
        .strip_prefix("gist create --public=false ")
        .expect("Pi's gist call");
    assert!(upload.ends_with("session.html"));
    assert!(!Path::new(upload).exists(), "the temporary file is removed");
    let html = place.recorded("uploaded.html").expect("uploaded");
    let data = decoded_session(&html);
    assert!(data.contains("hello there") && data.contains("general kenobi"));
    h.stop();
}

// Bake: declining, or Esc, uploads nothing; Pi's `Share cancelled` comes
// back with the draft.
#[test]
fn declining_a_share_uploads_nothing() {
    let place = Place::new("no");
    let h = share_harness(&place, place.gh());
    h.talk(1, "secret plans", "noted");
    h.submit_id(2, Submission::FollowUp("/share".into()));
    let dialog = h.sheet();
    h.choose(&dialog, "no");
    let updates = h.settled(2);
    assert_eq!(
        refused(&updates).as_deref(),
        Some("Share cancelled; your draft is back.")
    );
    h.submit_id(3, Submission::FollowUp("/share".into()));
    let dialog = h.sheet();
    h.reply_to(&dialog, DialogAnswer::Cancel);
    let updates = h.settled(3);
    assert_eq!(refused(&updates).as_deref(), Some(commands::CANCELLED));
    assert_eq!(place.calls(), ["auth status", "auth status"]);
    assert!(place.recorded("uploaded.html").is_none());
    h.stop();
}

// Pi's errors: a `gh` that is logged out or missing, and a session that
// is not written yet; none opens the confirmation.
#[test]
fn share_reports_pi_s_errors_before_asking() {
    let place = Place::new("errors");
    let h = share_harness(&place, place.gh());
    h.submit_id(1, Submission::FollowUp("/share".into()));
    let updates = h.settled(1);
    assert_eq!(
        refused(&updates).as_deref(),
        Some("Failed to export session: Nothing to export yet - start a conversation first")
    );
    place.mark("logged-out");
    h.talk(2, "hi", "hello");
    h.submit_id(3, Submission::FollowUp("/share".into()));
    let updates = h.settled(3);
    assert_eq!(
        refused(&updates).as_deref(),
        Some(copy::SHARE_GH_NOT_LOGGED_IN)
    );
    h.submit_id(4, Submission::FollowUp("/share now".into()));
    let updates = h.settled(4);
    assert_eq!(
        refused(&updates).as_deref(),
        Some("/share takes no argument; your draft is kept.")
    );
    assert!(
        !updates
            .iter()
            .any(|update| matches!(update, RuntimeUpdate::Dialog(Some(_))))
    );
    h.stop();
    assert!(place.recorded("uploaded.html").is_none());

    let missing = Place::new("missing");
    let h = share_harness(&missing, missing.0.join("no-such-gh"));
    h.talk(1, "hi", "hello");
    h.submit_id(2, Submission::FollowUp("/share".into()));
    let updates = h.settled(2);
    assert_eq!(refused(&updates).as_deref(), Some(copy::SHARE_GH_MISSING));
    h.stop();
}

// Bake: `/share` and `/bug` start only while the session is idle.
#[test]
fn share_and_bug_wait_for_an_idle_session() {
    let place = Place::new("busy");
    let h = share_harness(&place, place.gh());
    let release = Arc::new(Notify::new());
    let held = Arc::clone(&release);
    h.faux
        .set_responses(vec![FauxResponseStep::Factory(Arc::new(
            move |_, _, _, _| {
                let held = Arc::clone(&held);
                Box::pin(async move {
                    held.notified().await;
                    Ok(faux_assistant_message("done"))
                })
            },
        ))]);
    h.submit_id(1, Submission::FollowUp("working".into()));
    h.wait_for_requests(1);
    for (id, command) in [(2, "/share"), (3, "/bug it hangs")] {
        h.submit_id(id, Submission::FollowUp(command.into()));
        let updates = h.settled(id);
        assert!(
            refused(&updates).is_some_and(|reason| reason.starts_with("A turn is running")),
            "{updates:?}"
        );
    }
    release.notify_one();
    h.until_turn_end();
    h.stop();
    assert!(place.calls().is_empty());
}

// Pi `reportBug` with Bake's issue: the description sheet says where the
// report goes; the transcript stays out of the public issue; the body is
// redacted; the filed report is recorded in the session.
#[test]
fn bug_files_a_redacted_issue_and_records_it() {
    let place = Place::new("bug-issue");
    let h = share_harness(&place, place.gh());
    h.talk(1, "private words", "private answer");
    h.submit_id(2, Submission::FollowUp("/bug the edit tool failed".into()));
    let dialog = h.sheet();
    let DialogKind::Field(field) = &dialog.kind else {
        panic!("{dialog:?}");
    };
    assert_eq!(field.initial, "the edit tool failed");
    assert!(
        field
            .prompt
            .contains(&format!("public GitHub issue on {BAKE_REPOSITORY}"))
    );
    assert!(field.prompt.ends_with(copy::BUG_QUESTION));
    h.reply_to(
        &dialog,
        DialogAnswer::Entered(SecretText::from(
            "the edit tool failed\nwith a stack".to_owned(),
        )),
    );
    let dialog = h.sheet();
    assert_eq!(list_note(&dialog), copy::BUG_TRANSCRIPT_NOTE);
    h.choose(&dialog, "yes");
    let dialog = h.sheet();
    let summary = list_note(&dialog);
    assert!(summary.ends_with(
        "\nTranscript: in the zip archive only\nDescription: the edit tool failed\nwith a stack"
    ));
    let DialogKind::List(list) = &dialog.kind else {
        unreachable!()
    };
    assert_eq!(
        list.items[0].label,
        copy::BUG_CREATE_ISSUE_WITHOUT_TRANSCRIPT
    );
    // Nothing has run before the last choice.
    assert!(place.calls().is_empty());
    h.choose(&dialog, "issue");
    let updates = h.settled(2);
    assert!(admitted(&updates, 2));
    let shown = note(&updates).expect("note");
    assert!(
        shown.starts_with(
            "Bug report filed: https://github.com/Justar96/bake/issues/42\nReport ID: "
        ),
        "{shown}"
    );
    assert_eq!(place.recorded("repo").as_deref(), Some(BAKE_REPOSITORY));
    assert_eq!(
        place.recorded("title").as_deref(),
        Some("Bug report: the edit tool failed")
    );
    let body = place.recorded("body.md").expect("body");
    assert!(body.starts_with("the edit tool failed\nwith a stack\n\n---\nReport ID: "));
    assert!(body.contains("\"apiKey\": \"<redacted>\""));
    for secret in [
        "sk-settings-secret",
        "private words",
        "private answer",
        "test-key",
    ] {
        assert!(!body.contains(secret), "{secret} in the public issue");
    }
    let records: Vec<serde_json::Value> = place
        .session_lines()
        .into_iter()
        .filter(|line| line["customType"] == "bake.bug-report")
        .collect();
    assert_eq!(records.len(), 1);
    assert_eq!(records[0]["data"]["delivery"], "issue");
    assert_eq!(
        records[0]["data"]["url"],
        "https://github.com/Justar96/bake/issues/42"
    );
    h.stop();
}

// Pi: cancelling at the last choice sends nothing and records nothing.
#[test]
fn a_cancelled_bug_report_sends_nothing() {
    let place = Place::new("bug-cancel");
    let h = share_harness(&place, place.gh());
    h.talk(1, "hi", "hello");
    h.submit_id(2, Submission::FollowUp("/bug".into()));
    let dialog = h.sheet();
    h.reply_to(
        &dialog,
        DialogAnswer::Entered(SecretText::from(String::new())),
    );
    let dialog = h.sheet();
    h.choose(&dialog, "no");
    let dialog = h.sheet();
    assert!(list_note(&dialog).ends_with("\nTranscript: not included\nDescription: none"));
    h.choose(&dialog, "cancel");
    let updates = h.settled(2);
    assert_eq!(
        refused(&updates).as_deref(),
        Some("Bug report cancelled; your draft is back.")
    );
    // Esc on the first sheet cancels as every sheet does.
    h.submit_id(3, Submission::FollowUp("/bug".into()));
    let dialog = h.sheet();
    h.reply_to(&dialog, DialogAnswer::Cancel);
    let updates = h.settled(3);
    assert_eq!(refused(&updates).as_deref(), Some(commands::CANCELLED));
    assert!(place.calls().is_empty());
    assert!(
        !place
            .session_lines()
            .iter()
            .any(|line| line["customType"] == "bake.bug-report")
    );
    let archives = std::fs::read_dir(place.project()).expect("project").count();
    assert_eq!(archives, 0);
    h.stop();
}

// Pi's upload-failed fallback: the zip archive, with the transcript when
// it was included, in the working directory, recorded in the session; or
// Bake's prefilled link, which records nothing.
#[test]
fn a_failed_issue_falls_back_to_the_zip_or_a_link() {
    let place = Place::new("bug-fallback");
    place.mark("issue-fails");
    let h = share_harness(&place, place.gh());
    h.talk(1, "zip me", "zipped");
    h.submit_id(2, Submission::FollowUp("/bug broken".into()));
    let dialog = h.sheet();
    h.reply_to(
        &dialog,
        DialogAnswer::Entered(SecretText::from("broken".to_owned())),
    );
    let dialog = h.sheet();
    h.choose(&dialog, "yes");
    let dialog = h.sheet();
    h.choose(&dialog, "issue");
    let dialog = h.sheet();
    assert_eq!(
        list_note(&dialog),
        "Failed to create issue: HTTP 403: Resource not accessible\nExport the report as a zip archive instead?"
    );
    h.choose(&dialog, "zip");
    let updates = h.settled(2);
    let shown = note(&updates).expect("note");
    let path = shown
        .strip_prefix("Bug report exported to: ")
        .and_then(|rest| rest.split('\n').next())
        .expect("path");
    let archive = std::fs::read(path).expect("archive");
    assert!(Path::new(path).starts_with(place.project()));
    let text = String::from_utf8_lossy(&archive);
    assert!(text.contains("report.json") && text.contains("session.jsonl"));
    assert!(text.contains("zip me"), "the transcript is in the archive");
    assert!(!text.contains("sk-settings-secret"));
    let records: Vec<serde_json::Value> = place
        .session_lines()
        .into_iter()
        .filter(|line| line["customType"] == "bake.bug-report")
        .collect();
    assert_eq!(records.len(), 1);
    assert_eq!(records[0]["data"]["delivery"], "zip");
    assert_eq!(records[0]["data"]["sessionIncluded"], true);

    h.submit_id(3, Submission::FollowUp("/bug".into()));
    let dialog = h.sheet();
    h.reply_to(
        &dialog,
        DialogAnswer::Entered(SecretText::from("again".to_owned())),
    );
    let dialog = h.sheet();
    h.choose(&dialog, "no");
    let dialog = h.sheet();
    h.choose(&dialog, "issue");
    let dialog = h.sheet();
    h.choose(&dialog, "link");
    let updates = h.settled(3);
    let shown = note(&updates).expect("note");
    assert!(
        shown.starts_with(&format!(
            "{} https://github.com/{BAKE_REPOSITORY}/issues/new?title=Bug%20report%3A%20again&body=",
            copy::BUG_LINK
        )),
        "{shown}"
    );
    let count = place
        .session_lines()
        .iter()
        .filter(|line| line["customType"] == "bake.bug-report")
        .count();
    assert_eq!(count, 1, "a link files nothing, so nothing is recorded");
    h.stop();
}

// Pi `handleChangelogCommand`: the entries under `What's New`, with no
// sheet and no request.
#[test]
fn changelog_shows_the_shipped_entries() {
    let h = harness(Vec::new(), Vec::new());
    h.submit_id(1, Submission::FollowUp("/changelog".into()));
    let updates = h.settled(1);
    assert!(admitted(&updates, 1));
    let rows = committed(&updates);
    assert_eq!(rows[0], Row::Info("What's New".into()));
    let newest = commands::changelog::parse(commands::changelog::CHANGELOG)
        .into_iter()
        .next()
        .expect("a released entry");
    let heading = newest.content.lines().next().expect("a heading").to_owned();
    assert!(
        matches!(&rows[1], Row::Answer(text) if text.trim_end().ends_with(
        commands::changelog::normalize_links(&newest.content, &newest).as_str()
    ) && text.contains(&heading))
    );
    assert!(
        !updates
            .iter()
            .any(|update| matches!(update, RuntimeUpdate::Dialog(_)))
    );
    assert_eq!(h.faux.call_count(), 0);
    h.stop();
}

/// `text` with its `%XX` escapes decoded, as the browser reads a link.
fn percent_decoded(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut at = 0;
    while at < bytes.len() {
        let hex = bytes
            .get(at + 1..at + 3)
            .and_then(|hex| std::str::from_utf8(hex).ok())
            .and_then(|hex| u8::from_str_radix(hex, 16).ok());
        match (bytes[at], hex) {
            (b'%', Some(byte)) => {
                out.push(byte);
                at += 3;
            }
            (byte, _) => {
                out.push(byte);
                at += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

// Bake: the project's path, which Pi lists beside an included transcript,
// stays in the zip archive; neither the public issue's body nor the
// prefilled link names it, with the transcript or without, even when the
// description does.
#[test]
fn the_public_issue_never_names_the_project_path() {
    let place = Place::new("bug-paths");
    place.mark("issue-fails");
    let h = share_harness(&place, place.gh());
    h.talk(1, "hi", "hello");
    let project = place.project().to_string_lossy().into_owned();
    for (id, transcript) in [(2, "yes"), (3, "no")] {
        let _ = std::fs::remove_file(place.0.join("gh").join("body.md"));
        h.submit_id(id, Submission::FollowUp("/bug".into()));
        let dialog = h.sheet();
        h.reply_to(
            &dialog,
            DialogAnswer::Entered(SecretText::from(format!("{project}/src/a.rs breaks"))),
        );
        let dialog = h.sheet();
        h.choose(&dialog, transcript);
        let dialog = h.sheet();
        h.choose(&dialog, "issue");
        let dialog = h.sheet();
        h.choose(&dialog, "link");
        let updates = h.settled(id);
        let link = note(&updates).expect("the link");
        let body = place.recorded("body.md").expect("the issue body gh read");
        let title = place.recorded("title").expect("the title");
        assert!(body.starts_with("<project>/src/a.rs breaks"), "{body}");
        assert_eq!(title, "Bug report: <project>/src/a.rs breaks");
        let decoded = percent_decoded(&link);
        assert!(
            decoded.contains("body=<project>/src/a.rs breaks"),
            "{decoded}"
        );
        for text in [&body, &decoded, &title] {
            assert!(!text.contains(&project), "{transcript}: {text}");
            assert!(!text.contains("\"cwd\""), "{transcript}: {text}");
        }
    }
    h.stop();
}

// Bake: while `/share` or `/bug` reads the session, from its first sheet
// to the upload, a prompt is refused with its draft kept, so no turn
// starts and the upload holds only the settled session.
#[test]
fn share_and_bug_hold_prompts_until_they_end() {
    let place = Place::new("hold");
    let h = share_harness(&place, place.gh());
    h.talk(1, "hello there", "general kenobi");
    let calls = h.faux.call_count();
    h.submit_id(2, Submission::FollowUp("/share".into()));
    let dialog = h.sheet();
    h.submit_id(3, Submission::FollowUp("sneaky follow-up".into()));
    let updates = h.settled(3);
    assert_eq!(
        refused(&updates).as_deref(),
        Some("/share is reading the session; send this when it ends. Your draft is kept.")
    );
    h.choose(&dialog, "yes");
    assert!(admitted(&h.settled(2), 2));
    let data = decoded_session(&place.recorded("uploaded.html").expect("uploaded"));
    assert!(data.contains("general kenobi") && !data.contains("sneaky"));

    h.submit_id(4, Submission::FollowUp("/bug".into()));
    let dialog = h.sheet();
    h.reply_to(
        &dialog,
        DialogAnswer::Entered(SecretText::from("hangs".to_owned())),
    );
    let dialog = h.sheet();
    h.choose(&dialog, "yes");
    let dialog = h.sheet();
    h.submit_id(5, Submission::FollowUp("another prompt".into()));
    let updates = h.settled(5);
    assert!(
        refused(&updates).is_some_and(|reason| reason.starts_with("/bug is reading the session")),
        "{updates:?}"
    );
    h.choose(&dialog, "zip");
    let updates = h.settled(4);
    let shown = note(&updates).expect("note");
    let path = shown
        .strip_prefix("Bug report exported to: ")
        .and_then(|rest| rest.split('\n').next())
        .expect("path");
    let archive = String::from_utf8_lossy(&std::fs::read(path).expect("archive")).into_owned();
    assert!(archive.contains("general kenobi") && !archive.contains("another prompt"));
    assert_eq!(h.faux.call_count(), calls, "no turn started");
    // Once they end, prompts run again.
    h.talk(6, "after", "back");
    assert_eq!(h.faux.call_count(), calls + 1);
    h.stop();
}
