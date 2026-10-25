//! What `/share` and `/bug` read from a session, and the record `/bug`
//! leaves in it.
//!
//! Adapted from Pi v1.3.0 (`71023aec1546d6d677a65669c5665c0927ce0dd0`)
//! `packages/coding-agent/src/core/export-html/index.ts`
//! (`exportSessionToHtml`'s checks and session data),
//! `src/modes/interactive/bug-report.ts` (`buildBundle`, `recordInSession`),
//! and `src/modes/interactive/session-share.ts`
//! (`createShareTrailingEntries`). Reading takes the session lock briefly
//! and writes nothing; the record goes through the session's write
//! boundary as every other change does.

use std::path::Path;

use serde_json::Value;

use super::{AgentSession, lock};
use crate::bug_report::{self, BUG_REPORT_ENTRY_TYPE, BugReportBundle, MetadataInput};
use crate::export_html::{self, SessionData};
use crate::session_export;

/// Pi's refusal to export a session that is not saved.
pub const EXPORT_IN_MEMORY: &str = "Cannot export in-memory session to HTML";

/// Pi's refusal to export a session whose file is not written yet.
pub const EXPORT_NOTHING_YET: &str = "Nothing to export yet - start a conversation first";

/// What `/bug` reports besides the session.
pub struct BugReportRequest<'a> {
    /// The person's description.
    pub hint: Option<&'a str>,
    /// Whether the branch goes into the report (its archive only).
    pub include_session: bool,
    /// The Bake home's `settings.json` as stored, which the report redacts.
    pub global_settings: Option<Value>,
    /// [`bug_report::current_environment`] or a test's facts.
    pub environment: Value,
    /// The person's home directory, which the public issue names as `~`.
    pub home_dir: Option<String>,
}

impl AgentSession {
    fn tool_definitions(&self) -> Vec<Value> {
        self.agent
            .tools()
            .iter()
            .map(|tool| {
                export_html::tool_definition(&tool.name, &tool.description, &tool.parameters)
            })
            .collect()
    }

    /// Pi's `exportSessionToHtml` without the file write: the page for
    /// every entry of the session file, its leaf, and the session's system
    /// prompt and tools. Refused, with Pi's reasons, when the session is
    /// not saved or its file is not written yet. Blocking: it checks the
    /// file.
    pub fn export_html(&self) -> Result<String, String> {
        let file = {
            let session = lock(&self.session);
            session
                .is_persisted()
                .then(|| session.session_file().map(Path::to_path_buf))
                .flatten()
        };
        let Some(file) = file else {
            return Err(EXPORT_IN_MEMORY.to_owned());
        };
        if !file.exists() {
            return Err(EXPORT_NOTHING_YET.to_owned());
        }
        let system_prompt = self.agent.system_prompt();
        let tools = self.tool_definitions();
        let data = {
            let session = lock(&self.session);
            SessionData {
                header: session
                    .header()
                    .map(|header| Value::Object(header.as_json().clone())),
                entries: session
                    .entries()
                    .iter()
                    .map(|entry| Value::Object(entry.as_json().clone()))
                    .collect(),
                leaf_id: session.leaf_id().map(str::to_owned),
                system_prompt: Some(system_prompt),
                tools: Some(tools),
            }
        };
        Ok(export_html::generate_html(&data))
    }

    /// Pi's `buildBundle`: the redacted metadata, the failure diagnostics,
    /// and, when included, the branch as JSONL with the `pi.share` entry.
    pub fn bug_report_bundle(&self, request: BugReportRequest<'_>) -> BugReportBundle {
        let model = self.model();
        let auth_configured = model.as_ref().map(|model| {
            self.model_runtime()
                .snapshot()
                .has_configured_auth(&model.provider)
        });
        let message_count = self.agent.messages().len();
        let thinking = self.thinking_level();
        let system_prompt = self.agent.system_prompt();
        let tools = self.tool_definitions();
        let session = lock(&self.session);
        let metadata = bug_report::collect_metadata(MetadataInput {
            id: None,
            hint: request.hint,
            session_id: session.session_id(),
            cwd: session.cwd(),
            include_session: request.include_session,
            message_count,
            model: model.as_ref(),
            provider_auth_configured: auth_configured,
            thinking_level: thinking.as_str(),
            global_settings: request.global_settings.as_ref(),
            environment: request.environment,
        });
        let diagnostics = bug_report::collect_diagnostics(&session);
        let session_jsonl = request.include_session.then(|| {
            session_export::serialize_session_branch(&session, |parent, timestamp| {
                vec![session_export::share_trailing_entry(
                    parent,
                    timestamp,
                    &system_prompt,
                    tools,
                )]
            })
        });
        BugReportBundle {
            metadata,
            diagnostics,
            session_jsonl,
            cwd: session.cwd().to_owned(),
            home_dir: request.home_dir,
        }
    }

    /// Pi's `recordInSession`: a [`BUG_REPORT_ENTRY_TYPE`] custom entry
    /// with `data`, written as the other changes are; it is not part of the
    /// model's context. A failed write stops the session as any does.
    pub async fn record_bug_report(&self, data: Value) -> Result<(), String> {
        self.run_change(move |control| async move {
            let written = control
                .append(move |session| {
                    session
                        .append_custom_entry(BUG_REPORT_ENTRY_TYPE, Some(data))
                        .map(|_| ())
                })
                .await;
            if let Err(error) = written {
                return Err(control.stop_after_failed_write(&error, "The bug report record"));
            }
            Ok(())
        })
        .await
    }
}
