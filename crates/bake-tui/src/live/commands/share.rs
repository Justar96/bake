//! `/share`, adapted from Pi v1.3.0
//! `packages/coding-agent/src/modes/interactive/session-share.ts`
//! (`shareSession`'s gist path and `shareViaGist`), with the cases of
//! `test/session-share.test.ts` that apply to a gist.
//!
//! Bake has no Radius, so a share is always Pi's secret gist: `gh auth
//! status` must pass, the session is exported as Pi's HTML page
//! ([`AgentSession::export_html`]), and `gh gist create --public=false`
//! uploads it from a private temporary directory, which is removed
//! afterwards. Pi uploads without asking; Bake first asks, on a sheet that
//! says what the gist holds and who can read it, and uploads nothing unless
//! the person says yes. Bake has no session viewer like Pi's, so the note
//! names the gist alone. Esc on the sheet, while the upload runs, kills
//! `gh` and reports Pi's `Share cancelled`.

use std::sync::Arc;

use bake_tui_view::copy;
use bake_tui_view::dialog::{DialogAnswer, DialogKind, ListDialog, ListItem};

use super::gh::{self, Ran};
use super::{CANCELLED, Context, Done, Outcome};

/// Why a share ended with nothing uploaded: Pi's `Share cancelled`, and the
/// draft is back.
fn cancelled() -> String {
    format!("{}; your draft is back.", copy::SHARE_CANCELLED)
}

/// The yes-or-no sheet before an upload.
pub(super) fn confirmation() -> DialogKind {
    DialogKind::List(ListDialog {
        title: copy::SHARE_CONFIRM_TITLE.into(),
        items: vec![
            ListItem {
                value: "yes".into(),
                label: copy::SHARE_CONFIRM_YES.into(),
                ..ListItem::default()
            },
            ListItem {
                value: "no".into(),
                label: copy::SHARE_CONFIRM_NO.into(),
                ..ListItem::default()
            },
        ],
        current: None,
        // "No" is selected, so a stray Enter uploads nothing.
        selected: Some(1),
        filterable: false,
        saves_default: false,
        note: Some(copy::SHARE_CONFIRM_NOTE.into()),
        search: None,
    })
}

pub(super) async fn share(context: &mut Context) -> Outcome {
    let program = context.gh.clone();
    if !gh::check_auth(&program, &context.signal, &context.session).await? {
        return Err(CANCELLED.into());
    }
    let session = Arc::clone(&context.session);
    let html = tokio::task::spawn_blocking(move || session.export_html())
        .await
        .map_err(|error| error.to_string())?
        .map_err(|error| format!("{} {error}", copy::SHARE_EXPORT_FAILED))?;
    let answer = context.ask(confirmation()).await;
    match answer {
        DialogAnswer::Chose { value, .. } if value == "yes" => {}
        DialogAnswer::Chose { .. } => return Err(cancelled()),
        _ => return Err(CANCELLED.into()),
    }
    if context.signal.aborted() {
        return Err(cancelled());
    }
    let dir = tokio::task::spawn_blocking(move || {
        let dir = gh::TempDir::new("bake-share-")?;
        std::fs::write(dir.path().join("session.html"), html)?;
        Ok::<_, std::io::Error>(dir)
    })
    .await
    .map_err(|error| error.to_string())?
    .map_err(|error| format!("{} {error}", copy::SHARE_EXPORT_FAILED))?;
    let file = dir.path().join("session.html");
    let ran = gh::run(
        &program,
        &[
            "gist".into(),
            "create".into(),
            "--public=false".into(),
            file.into_os_string(),
        ],
        &context.signal,
        &context.session,
    )
    .await;
    // The directory goes once `gh` has read it, whatever it did.
    drop(dir);
    let (stdout, stderr, code) = match ran {
        Ok(Ran::Exited {
            code,
            stdout,
            stderr,
        }) => (stdout, stderr, code),
        Ok(Ran::Cancelled) => return Err(cancelled()),
        Err(error) => {
            return Err(format!("{} {}", copy::SHARE_GIST_FAILED, error.message()));
        }
    };
    if code != Some(0) {
        let reason = Some(stderr.trim())
            .filter(|reason| !reason.is_empty())
            .unwrap_or(copy::UNKNOWN_ERROR);
        return Err(format!("{} {reason}", copy::SHARE_GIST_FAILED));
    }
    let url = stdout.trim();
    let id = url.rsplit('/').next().unwrap_or("");
    if id.is_empty() {
        return Err(copy::SHARE_GIST_UNPARSED.into());
    }
    Ok(Done::Note(format!("{} {url}", copy::SHARE_GIST)))
}
