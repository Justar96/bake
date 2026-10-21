//! `/bug [description]`, adapted from Pi v1.3.0
//! `packages/coding-agent/src/modes/interactive/bug-report.ts`
//! (`reportBug`, `promptForOptions`, `buildBundle`, `upload`, `exportZip`,
//! `recordInSession`) and `src/core/bug-report-upload.ts`, with
//! `test/interactive-mode-bug-report-hint.test.ts`'s flow.
//!
//! Pi's report goes privately to its developers; Bake's goes to Bake's
//! repository ([`BAKE_REPOSITORY`]) as a public GitHub issue, filed with
//! `gh issue create`. The flow is Pi's: the description, whose sheet says
//! where the report goes and what it holds; whether to include the
//! transcript; then a summary with the choice to file, export a zip, or
//! cancel. Nothing leaves the machine before that last choice. The
//! transcript never goes into the public issue, only into the zip archive.
//! A failed filing offers Pi's zip fallback, and a prefilled new-issue link
//! the person can open themselves. A filed or exported report is recorded
//! in the session as Pi records it
//! ([`bake_coding_agent::bug_report::BUG_REPORT_ENTRY_TYPE`]), which the
//! model never sees. Pi's model-written summary is not ported.

use std::path::PathBuf;
use std::sync::Arc;

use bake_coding_agent::agent_session::share::BugReportRequest;
use bake_coding_agent::bug_report::{self, BAKE_REPOSITORY, BugReportBundle, Delivery};
use bake_tui_view::copy;
use bake_tui_view::dialog::{DialogAnswer, DialogKind, FieldDialog, ListDialog, ListItem};

use super::gh::{self, Ran};
use super::{CANCELLED, Context, Done, Outcome};

/// Why a report ended with nothing sent: Pi's `Bug report cancelled`, and
/// the draft is back.
fn cancelled() -> String {
    format!("{}; your draft is back.", copy::BUG_CANCELLED)
}

fn list(title: &str, note: String, items: &[(&str, &str)]) -> DialogKind {
    DialogKind::List(ListDialog {
        title: title.into(),
        items: items
            .iter()
            .map(|(value, label)| ListItem {
                value: (*value).into(),
                label: (*label).into(),
                ..ListItem::default()
            })
            .collect(),
        current: None,
        selected: None,
        filterable: false,
        saves_default: false,
        note: Some(note),
        search: None,
    })
}

fn repository(text: &str) -> String {
    text.replace("{repository}", BAKE_REPOSITORY)
}

/// What the person chose before anything is built.
struct Options {
    hint: Option<String>,
    include_session: bool,
    issue: bool,
}

/// The last sheet's note: where the report goes and whether the
/// transcript goes with it come first, so a sheet that cuts the note's
/// rows cuts the person's own description, never them.
fn delivery_summary(hint: Option<&str>, include_session: bool) -> String {
    format!(
        "{}\nTranscript: {}\nDescription: {}",
        repository(copy::BUG_DELIVERY_ISSUE_NOTE),
        if include_session {
            "in the zip archive only"
        } else {
            "not included"
        },
        hint.unwrap_or("none"),
    )
}

/// Pi's `promptForOptions`; why not, when the person cancelled: Esc gives
/// [`CANCELLED`], and the `Cancel` choice Pi's `Bug report cancelled`.
async fn options(context: &mut Context, initial: &str) -> Result<Options, String> {
    let DialogAnswer::Entered(text) = context
        .ask(DialogKind::Field(FieldDialog {
            title: copy::BUG_TITLE.into(),
            prompt: format!(
                "{}\n{}",
                repository(copy::BUG_DISCLAIMER),
                copy::BUG_QUESTION
            ),
            secret: false,
            initial: initial.to_owned(),
            error: None,
        }))
        .await
    else {
        return Err(CANCELLED.into());
    };
    let hint = Some(text.expose().trim().to_owned()).filter(|hint| !hint.is_empty());
    let DialogAnswer::Chose { value, .. } = context
        .ask(list(
            copy::BUG_TRANSCRIPT_TITLE,
            copy::BUG_TRANSCRIPT_NOTE.into(),
            &[
                ("yes", copy::BUG_TRANSCRIPT_YES),
                ("no", copy::BUG_TRANSCRIPT_NO),
            ],
        ))
        .await
    else {
        return Err(CANCELLED.into());
    };
    let include_session = value == "yes";
    let summary = delivery_summary(hint.as_deref(), include_session);
    let issue_label = if include_session {
        copy::BUG_CREATE_ISSUE_WITHOUT_TRANSCRIPT
    } else {
        copy::BUG_CREATE_ISSUE
    };
    let DialogAnswer::Chose { value, .. } = context
        .ask(list(
            copy::BUG_DELIVERY_TITLE,
            summary,
            &[
                ("issue", issue_label),
                ("zip", copy::BUG_EXPORT_ZIP),
                ("cancel", copy::BUG_CANCEL),
            ],
        ))
        .await
    else {
        return Err(CANCELLED.into());
    };
    let issue = match value.as_str() {
        "issue" => true,
        "zip" => false,
        _ => return Err(cancelled()),
    };
    Ok(Options {
        hint,
        include_session,
        issue,
    })
}

/// The Bake home's `settings.json` as stored, for the report to redact;
/// `None` when there is none or it is not a JSON object. Blocking.
fn global_settings(home: Option<&std::path::Path>) -> Option<serde_json::Value> {
    let path = bake_coding_agent::settings::settings_path(home?);
    let size = std::fs::metadata(&path).ok()?.len();
    if size > bake_coding_agent::settings::MAX_SETTINGS_FILE_BYTES {
        return None;
    }
    let text = std::fs::read_to_string(path).ok()?;
    serde_json::from_str::<serde_json::Value>(text.trim_start_matches('\u{feff}'))
        .ok()
        .filter(serde_json::Value::is_object)
}

pub(super) async fn bug(context: &mut Context, argument: &str) -> Outcome {
    let options = options(context, argument).await?;
    let session = Arc::clone(&context.session);
    let home = context.home.clone();
    let hint = options.hint.clone();
    let include_session = options.include_session;
    let bundle = tokio::task::spawn_blocking(move || {
        session.bug_report_bundle(BugReportRequest {
            hint: hint.as_deref(),
            include_session,
            global_settings: global_settings(home.as_deref()),
            environment: bug_report::current_environment(),
            home_dir: std::env::home_dir().map(|home| home.to_string_lossy().into_owned()),
        })
    })
    .await
    .map_err(|error| format!("{} {error}", copy::BUG_BUILD_FAILED))?;
    if context.signal.aborted() {
        return Err(cancelled());
    }
    if options.issue {
        let failure = match file_issue(context, &bundle).await {
            Filed::Done(outcome) => return outcome,
            Filed::Failed(failure) => failure,
        };
        let DialogAnswer::Chose { value, .. } = context
            .ask(list(
                copy::BUG_UPLOAD_FAILED_TITLE,
                format!("{failure}\n{}", copy::BUG_UPLOAD_FAILED_QUESTION),
                &[
                    ("zip", copy::BUG_EXPORT_ZIP),
                    ("link", copy::BUG_ISSUE_LINK),
                    ("cancel", copy::BUG_CANCEL),
                ],
            ))
            .await
        else {
            return Err(cancelled());
        };
        match value.as_str() {
            "zip" => {}
            "link" => {
                return Ok(Done::Note(format!(
                    "{} {}",
                    copy::BUG_LINK,
                    bug_report::new_issue_url(&bundle)
                )));
            }
            _ => return Err(cancelled()),
        }
    }
    export_zip(context, bundle).await
}

enum Filed {
    /// Filed, or cancelled: what the command ends with.
    Done(Outcome),
    /// Why filing failed, for the fallback sheet.
    Failed(String),
}

/// Pi's `upload`, as a GitHub issue: `gh auth status`, then `gh issue
/// create` with the body in a private temporary file.
async fn file_issue(context: &mut Context, bundle: &BugReportBundle) -> Filed {
    let program = context.gh.clone();
    match gh::check_auth(&program, &context.signal, &context.session).await {
        Ok(true) => {}
        Ok(false) => return Filed::Done(Err(cancelled())),
        Err(failure) => return Filed::Failed(failure),
    }
    let body = bug_report::issue_body(bundle);
    let dir = match tokio::task::spawn_blocking(move || {
        let dir = gh::TempDir::new("bake-bug-")?;
        std::fs::write(dir.path().join("issue.md"), body)?;
        Ok::<_, std::io::Error>(dir)
    })
    .await
    {
        Ok(Ok(dir)) => dir,
        Ok(Err(error)) => return Filed::Failed(error.to_string()),
        Err(error) => return Filed::Failed(error.to_string()),
    };
    let ran = gh::run(
        &program,
        &[
            "issue".into(),
            "create".into(),
            "--repo".into(),
            BAKE_REPOSITORY.into(),
            "--title".into(),
            bug_report::issue_title(bundle).into(),
            "--body-file".into(),
            dir.path().join("issue.md").into_os_string(),
        ],
        &context.signal,
        &context.session,
    )
    .await;
    drop(dir);
    let stdout = match ran {
        Ok(Ran::Exited {
            code: Some(0),
            stdout,
            ..
        }) => stdout,
        Ok(Ran::Exited { stderr, .. }) => {
            let reason = Some(stderr.trim())
                .filter(|reason| !reason.is_empty())
                .unwrap_or(copy::UNKNOWN_ERROR);
            return Filed::Failed(format!("{} {reason}", copy::BUG_ISSUE_FAILED));
        }
        Ok(Ran::Cancelled) => return Filed::Done(Err(cancelled())),
        Err(error) => return Filed::Failed(error.message()),
    };
    let Some(url) = stdout
        .lines()
        .map(str::trim)
        .rev()
        .find(|line| line.starts_with("https://"))
    else {
        // `gh` said it filed the issue: report it rather than offer to
        // file it again.
        return Filed::Done(Ok(Done::Note(format!(
            "{} {}",
            copy::BUG_ISSUE_UNPARSED,
            bundle.id()
        ))));
    };
    let note = format!(
        "{} {url}\n{} {}",
        copy::BUG_ISSUE_CREATED,
        copy::BUG_REPORT_ID,
        bundle.id()
    );
    Filed::Done(Ok(Done::Note(
        record(context, bundle, Delivery::Issue { url }, note).await,
    )))
}

/// Pi's `exportZip`: the archive in the session's working directory.
async fn export_zip(context: &mut Context, bundle: BugReportBundle) -> Outcome {
    let path =
        PathBuf::from(context.session.cwd()).join(bug_report::archive_file_name(bundle.id()));
    let written = {
        let (bundle, path) = (bundle.clone(), path.clone());
        tokio::task::spawn_blocking(move || bug_report::write_archive(&bundle, &path))
            .await
            .map_err(|error| error.to_string())
            .and_then(|written| written.map_err(|error| error.to_string()))
    };
    if let Err(error) = written {
        return Err(format!("{} {error}", copy::BUG_WRITE_FAILED));
    }
    let note = format!(
        "{} {}\n{} {}",
        copy::BUG_EXPORTED,
        path.display(),
        copy::BUG_REPORT_ID,
        bundle.id()
    );
    Ok(Done::Note(
        record(context, &bundle, Delivery::Zip { path: &path }, note).await,
    ))
}

/// Pi's `recordInSession`, after the report went out: its failure is added
/// to the note rather than hiding that the report was sent.
async fn record(
    context: &Context,
    bundle: &BugReportBundle,
    delivery: Delivery<'_>,
    note: String,
) -> String {
    match context
        .session
        .record_bug_report(bundle.entry_data(delivery))
        .await
    {
        Ok(()) => note,
        Err(error) => format!("{note}\n{} {error}", copy::BUG_NOT_RECORDED),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Bake: a sheet shows at most six rows of its note, wrapped to the
    // sheet's inner width (`render::sheet_text`, four columns inside the
    // sheet), so on a 40-column terminal where the report goes and whether
    // the transcript goes with it are read before a long description,
    // which alone is cut.
    #[test]
    fn the_delivery_summary_keeps_where_the_report_goes_first() {
        let description = "the edit tool failed ".repeat(40);
        for include_session in [true, false] {
            let summary = delivery_summary(Some(&description), include_session);
            let shown: Vec<String> = summary
                .split('\n')
                .flat_map(|line| bake_tui_view::transcript::wrap(line, 36))
                .take(6)
                .collect();
            let shown = shown.join(" ");
            assert!(
                shown.contains(&format!("public issue on {BAKE_REPOSITORY}")),
                "{shown}"
            );
            assert!(shown.contains("Transcript: "), "{shown}");
            assert!(shown.contains("Description: the edit tool"), "{shown}");
        }
        assert!(delivery_summary(None, false).ends_with("Description: none"));
    }
}
