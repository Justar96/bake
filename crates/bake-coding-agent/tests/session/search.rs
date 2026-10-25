//! The resume picker's search over real listings: Pi `buildSessionInfo`
//! (`allMessagesText`) feeding `session-selector-search.ts`. Pi has no test
//! that joins the two; these follow its sources (v1.2.0).

use bake_coding_agent::session::search::{NameFilter, SessionSearchIndex, SortMode};
use bake_coding_agent::session::{NewSessionOptions, SessionManager};
use serde_json::json;

use crate::support::{TempDir, assistant_msg, message, usage, user_msg};

/// Ids the listing in `dir` finds for `query`, newest-first order kept.
fn found(dir: &TempDir, query: &str) -> Vec<String> {
    let listed = SessionManager::list(dir.str(), Some(dir.path()), None, None).expect("list");
    let index = SessionSearchIndex::new(&listed);
    index
        .search(query, SortMode::Recent, NameFilter::All)
        .indexes
        .into_iter()
        .map(|i| listed[i].id.clone())
        .collect()
}

/// Every user and assistant text block in the file is searched, later
/// messages and abandoned branches included; tool results, thinking, and
/// tool calls are not. Searching writes nothing.
#[test]
fn searches_every_user_and_assistant_text_in_the_log() {
    let dir = TempDir::new("search-listing");
    let mut session =
        SessionManager::create(dir.str(), Some(dir.path()), NewSessionOptions::default())
            .expect("create");
    let first = session
        .append_message(user_msg("first ask"))
        .expect("append");
    session
        .append_message(message(json!({
            "role": "assistant",
            "content": [
                {"type": "thinking", "thinking": "hidden marmot"},
                {"type": "text", "text": "visible"},
                {"type": "toolCall", "id": "c1", "name": "read", "arguments": {"path": "ocelot.txt"}},
            ],
            "api": "anthropic-messages", "provider": "anthropic", "model": "test",
            "usage": usage(), "stopReason": "toolUse", "timestamp": 2,
        })))
        .expect("append");
    session
        .append_message(message(json!({
            "role": "toolResult", "toolCallId": "c1", "toolName": "read",
            "content": [{"type": "text", "text": "secret pangolin"}],
            "isError": false, "timestamp": 3,
        })))
        .expect("append");
    session
        .append_message(assistant_msg("abandoned\n\n  wombat branch"))
        .expect("append");
    // Back to the first ask: the branch above stays in the file.
    session.branch(&first).expect("branch");
    for n in 0..20 {
        session
            .append_message(user_msg(&format!("filler {n}")))
            .expect("append");
    }
    session
        .append_message(assistant_msg("Much later Quokka answer"))
        .expect("append");
    session.append_session_info("Release prep").expect("name");
    let id = session.session_id().to_owned();
    let file = session.session_file().expect("file").to_path_buf();
    drop(session);

    let mut other =
        SessionManager::create(dir.str(), Some(dir.path()), NewSessionOptions::default())
            .expect("create");
    other.append_message(user_msg("unrelated")).expect("append");
    drop(other);
    let before = std::fs::read(&file).expect("read");

    assert_eq!(found(&dir, "\"later quokka\""), [id.as_str()]);
    assert_eq!(found(&dir, "\"abandoned wombat branch\""), [id.as_str()]);
    assert_eq!(found(&dir, "re:QUOKKA\\s+answer"), [id.as_str()]);
    assert_eq!(found(&dir, "\"release prep\""), [id.as_str()]);
    assert_eq!(found(&dir, &format!("\"{id}\"")), [id.as_str()]);
    for absent in ["\"hidden marmot\"", "\"secret pangolin\"", "\"ocelot.txt\""] {
        assert!(found(&dir, absent).is_empty(), "{absent}");
    }
    // The working directory is searched too, so it matches both.
    let dir_name = dir
        .path()
        .file_name()
        .and_then(|name| name.to_str())
        .expect("name");
    assert_eq!(found(&dir, &format!("\"{dir_name}\"")).len(), 2);
    assert_eq!(std::fs::read(&file).expect("read"), before);
}
