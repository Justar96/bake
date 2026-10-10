//! Pi `test/session-manager/file-operations.test.ts`.

use std::fs;
use std::io::{Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::time::{Duration, SystemTime};

use bake_coding_agent::session::{
    FileEntry, NewSessionOptions, SessionError, SessionManager, find_most_recent_session,
    load_entries_from_file,
};
use serde_json::json;

use crate::support::{TempDir, assistant_msg, read_session_file_roles, text_of, user_msg};

const HEADER_SCAN_LIMIT_BYTES: usize = 1024 * 1024;
const VALID_HEADER: &str =
    r#"{"type":"session","id":"abc","timestamp":"2025-01-01T00:00:00Z","cwd":"/tmp"}"#;
const VALID_MESSAGE: &str = r#"{"type":"message","id":"1","parentId":null,"timestamp":"2025-01-01T00:00:01Z","message":{"role":"user","content":"hi","timestamp":1}}"#;

fn write_session_header(file: &Path, cwd: &str, id: &str, prefix: &str) {
    let header = json!({
        "type": "session", "version": 3, "id": id, "timestamp": "2025-01-01T00:00:00Z", "cwd": cwd,
    });
    fs::write(file, format!("{prefix}{header}\n")).expect("write header");
}

fn set_mtime(path: &Path, seconds_ago: u64) {
    let time = SystemTime::now() - Duration::from_secs(seconds_ago);
    fs::File::options()
        .write(true)
        .open(path)
        .and_then(|file| file.set_modified(time))
        .expect("set mtime");
}

mod load_entries_from_file_tests {
    use super::*;

    /// "returns empty array for non-existent file"
    #[test]
    fn returns_empty_for_missing_file() {
        let dir = TempDir::new("load-missing");
        assert!(
            load_entries_from_file(&dir.join("nonexistent.jsonl"))
                .expect("load")
                .is_empty()
        );
    }

    /// "returns empty array for empty file"
    #[test]
    fn returns_empty_for_empty_file() {
        let dir = TempDir::new("load-empty");
        let file = dir.join("empty.jsonl");
        fs::write(&file, "").expect("write");
        assert!(load_entries_from_file(&file).expect("load").is_empty());
    }

    /// "returns empty array for file without valid session header"
    #[test]
    fn returns_empty_without_valid_header() {
        let dir = TempDir::new("load-no-header");
        let file = dir.join("no-header.jsonl");
        fs::write(&file, "{\"type\":\"message\",\"id\":\"1\"}\n").expect("write");
        assert!(load_entries_from_file(&file).expect("load").is_empty());
    }

    /// "returns empty array for malformed JSON"
    #[test]
    fn returns_empty_for_malformed_json() {
        let dir = TempDir::new("load-malformed");
        let file = dir.join("malformed.jsonl");
        fs::write(&file, "not json\n").expect("write");
        assert!(load_entries_from_file(&file).expect("load").is_empty());
    }

    /// "loads valid session file"
    #[test]
    fn loads_valid_session_file() {
        let dir = TempDir::new("load-valid");
        let file = dir.join("valid.jsonl");
        fs::write(&file, format!("{VALID_HEADER}\n{VALID_MESSAGE}\n")).expect("write");
        let entries = load_entries_from_file(&file).expect("load");
        assert_eq!(entries.len(), 2);
        assert!(matches!(entries[0], FileEntry::Header(_)));
        assert_eq!(
            entries[1].as_entry().map(|entry| entry.entry_type()),
            Some("message")
        );
    }

    /// "skips malformed lines but keeps valid ones"
    #[test]
    fn skips_malformed_lines() {
        let dir = TempDir::new("load-mixed");
        let file = dir.join("mixed.jsonl");
        fs::write(
            &file,
            format!("{VALID_HEADER}\nnot valid json\n{VALID_MESSAGE}\n"),
        )
        .expect("write");
        assert_eq!(load_entries_from_file(&file).expect("load").len(), 2);
    }

    /// "adds a newline after an unterminated valid record"
    #[test]
    fn adds_newline_after_unterminated_valid_record() {
        let dir = TempDir::new("load-unterminated");
        let file = dir.join("unterminated.jsonl");
        let content = format!("{VALID_HEADER}\n{VALID_MESSAGE}");
        fs::write(&file, &content).expect("write");
        assert_eq!(load_entries_from_file(&file).expect("load").len(), 2);
        assert_eq!(
            fs::read_to_string(&file).expect("read"),
            format!("{content}\n")
        );
    }

    /// "adds a newline after an unterminated malformed final fragment"
    #[test]
    fn adds_newline_after_torn_final_fragment() {
        let dir = TempDir::new("load-torn");
        let file = dir.join("malformed-tail.jsonl");
        let content = format!("{VALID_HEADER}\n{{\"type\":\"message\"");
        fs::write(&file, &content).expect("write");
        assert_eq!(load_entries_from_file(&file).expect("load").len(), 1);
        assert_eq!(
            fs::read_to_string(&file).expect("read"),
            format!("{content}\n")
        );
    }

    /// "does not modify an unterminated non-session file"
    #[test]
    fn does_not_modify_unterminated_non_session_file() {
        let dir = TempDir::new("load-invalid");
        let file = dir.join("invalid.jsonl");
        let content = "{\"type\":\"message\",\"id\":\"1\"}";
        fs::write(&file, content).expect("write");
        assert!(load_entries_from_file(&file).expect("load").is_empty());
        assert_eq!(fs::read_to_string(&file).expect("read"), content);
    }

    /// "reads cwd from a session with %s": leading blank lines, leading
    /// malformed lines, and a multi-buffer header.
    #[test]
    fn reads_cwd_past_blank_malformed_and_long_prefixes() {
        let long_id = "a".repeat(8192);
        for (prefix, id) in [
            ("\n  \n", "leading-blank"),
            ("not json\n{broken json\n", "leading-malformed"),
            ("", long_id.as_str()),
        ] {
            let dir = TempDir::new("load-header-cwd");
            let file = dir.join("header.jsonl");
            let stored_cwd = dir.join("stored-project");
            let stored_cwd = stored_cwd.to_str().expect("UTF-8");
            write_session_header(&file, stored_cwd, id, prefix);
            let session = SessionManager::open(&file, Some(dir.path()), None).expect("open");
            assert_eq!(session.session_id(), id);
            assert_eq!(session.cwd(), stored_cwd);
        }
    }

    /// "opens compatible sessions beyond the discovery scan limit"
    #[test]
    fn opens_sessions_beyond_the_discovery_scan_limit() {
        let dir = TempDir::new("load-scan-limit");
        let stored_cwd = dir.join("stored-project");
        let override_cwd = dir.join("override-project");
        let large_id = "a".repeat(HEADER_SCAN_LIMIT_BYTES + 1);
        let large_prefix = format!("{}\n", "x".repeat(HEADER_SCAN_LIMIT_BYTES + 1));
        for (name, id, prefix) in [
            ("large-header", large_id.as_str(), ""),
            ("large-prefix", "large-prefix", large_prefix.as_str()),
        ] {
            let file = dir.join(&format!("{name}.jsonl"));
            write_session_header(&file, stored_cwd.to_str().expect("UTF-8"), id, prefix);
            for cwd_override in [None, override_cwd.to_str()] {
                let session =
                    SessionManager::open(&file, Some(dir.path()), cwd_override).expect("open");
                assert_eq!(session.session_id(), id);
                assert_eq!(
                    session.cwd(),
                    cwd_override.unwrap_or(stored_cwd.to_str().expect("UTF-8"))
                );
            }
        }
    }

    /// "opens session files larger than Node's max string length". Rust has
    /// no string length limit, so this keeps the shape at a test-friendly
    /// size: a sparse file of long malformed lines between the header and a
    /// valid entry.
    #[test]
    fn opens_files_with_long_malformed_lines() {
        let dir = TempDir::new("load-large");
        let file = dir.join("large.jsonl");
        fs::write(
            &file,
            "{\"type\":\"session\",\"version\":3,\"id\":\"abc\",\"timestamp\":\"2025-01-01T00:00:00Z\",\"cwd\":\"/tmp\"}\n",
        )
        .expect("write");
        {
            let mut handle = fs::File::options().write(true).open(&file).expect("open");
            let stride = 4 * 1024 * 1024;
            for offset in (1..=4).map(|index| index * stride) {
                handle.seek(SeekFrom::Start(offset)).expect("seek");
                handle.write_all(b"\n").expect("write");
            }
        }
        fs::OpenOptions::new()
            .append(true)
            .open(&file)
            .and_then(|mut handle| handle.write_all(format!("{VALID_MESSAGE}\n").as_bytes()))
            .expect("append");
        let session = SessionManager::open(&file, Some(dir.path()), None).expect("open");
        assert_eq!(session.session_id(), "abc");
        assert_eq!(session.entries().len(), 1);
        let messages = session.build_session_context().messages;
        assert_eq!(
            messages
                .iter()
                .map(|message| serde_json::to_string(message).unwrap_or_default())
                .collect::<Vec<_>>(),
            [r#"{"role":"user","content":"hi","timestamp":1}"#]
        );
    }
}

mod find_most_recent_session_tests {
    use super::*;

    /// "returns null for empty directory"
    #[test]
    fn none_for_empty_directory() {
        let dir = TempDir::new("recent-empty");
        assert_eq!(find_most_recent_session(dir.path(), None), None);
    }

    /// "returns null for non-existent directory"
    #[test]
    fn none_for_missing_directory() {
        let dir = TempDir::new("recent-missing");
        assert_eq!(
            find_most_recent_session(&dir.join("nonexistent"), None),
            None
        );
    }

    /// "ignores non-jsonl files"
    #[test]
    fn ignores_non_jsonl_files() {
        let dir = TempDir::new("recent-non-jsonl");
        fs::write(dir.join("file.txt"), "hello").expect("write");
        fs::write(dir.join("file.json"), "{}").expect("write");
        assert_eq!(find_most_recent_session(dir.path(), None), None);
    }

    /// "ignores jsonl files without valid session header"
    #[test]
    fn ignores_files_without_header() {
        let dir = TempDir::new("recent-invalid");
        fs::write(dir.join("invalid.jsonl"), "{\"type\":\"message\"}\n").expect("write");
        assert_eq!(find_most_recent_session(dir.path(), None), None);
    }

    /// "returns single valid session file"
    #[test]
    fn returns_single_valid_file() {
        let dir = TempDir::new("recent-single");
        let file = dir.join("session.jsonl");
        fs::write(&file, format!("{VALID_HEADER}\n")).expect("write");
        assert_eq!(find_most_recent_session(dir.path(), None), Some(file));
    }

    /// "returns most recently modified session"
    #[test]
    fn returns_most_recently_modified() {
        let dir = TempDir::new("recent-order");
        let older = dir.join("older.jsonl");
        let newer = dir.join("newer.jsonl");
        fs::write(&older, VALID_HEADER.replace("abc", "old") + "\n").expect("write");
        fs::write(&newer, VALID_HEADER.replace("abc", "new") + "\n").expect("write");
        set_mtime(&older, 20);
        set_mtime(&newer, 10);
        assert_eq!(find_most_recent_session(dir.path(), None), Some(newer));
    }

    /// "skips invalid files and returns valid one"
    #[test]
    fn skips_invalid_files() {
        let dir = TempDir::new("recent-skip");
        let invalid = dir.join("invalid.jsonl");
        let valid = dir.join("valid.jsonl");
        fs::write(&invalid, "{\"type\":\"not-session\"}\n").expect("write");
        fs::write(&valid, format!("{VALID_HEADER}\n")).expect("write");
        set_mtime(&valid, 20);
        set_mtime(&invalid, 10);
        assert_eq!(find_most_recent_session(dir.path(), None), Some(valid));
    }

    /// "skips oversized corrupt files and returns a valid session"
    #[test]
    fn skips_oversized_corrupt_files() {
        let dir = TempDir::new("recent-oversized");
        let invalid = dir.join("oversized.jsonl");
        let valid = dir.join("valid.jsonl");
        fs::write(&invalid, "x".repeat(HEADER_SCAN_LIMIT_BYTES + 1)).expect("write");
        fs::write(&valid, format!("{VALID_HEADER}\n")).expect("write");
        set_mtime(&valid, 20);
        set_mtime(&invalid, 10);
        assert_eq!(find_most_recent_session(dir.path(), None), Some(valid));
    }

    /// "filters most recent session by cwd"
    #[test]
    fn filters_by_cwd() {
        let dir = TempDir::new("recent-cwd");
        let project_a = dir.join("project-a");
        let project_b = dir.join("project-b");
        let file_a = dir.join("a.jsonl");
        let file_b = dir.join("b.jsonl");
        for (file, id, cwd) in [(&file_a, "a", &project_a), (&file_b, "b", &project_b)] {
            let header = json!({"type": "session", "id": id, "timestamp": "2025-01-01T00:00:00Z", "cwd": cwd});
            fs::write(file, format!("{header}\n")).expect("write");
        }
        set_mtime(&file_a, 20);
        set_mtime(&file_b, 10);
        assert_eq!(
            find_most_recent_session(dir.path(), project_a.to_str()),
            Some(file_a)
        );
        assert_eq!(
            find_most_recent_session(dir.path(), project_b.to_str()),
            Some(file_b)
        );
    }
}

mod custom_flat_session_directory {
    use super::*;

    fn create_persisted_session(cwd: &Path, dir: &Path, label: &str) -> PathBuf {
        let mut session = SessionManager::create(
            cwd.to_str().expect("UTF-8"),
            Some(dir),
            NewSessionOptions::default(),
        )
        .expect("create");
        session.append_message(user_msg(label)).expect("append");
        session
            .append_message(assistant_msg(&format!("reply to {label}")))
            .expect("append");
        session.session_file().expect("file").to_path_buf()
    }

    /// "scopes current-folder APIs by cwd while listing all flat sessions"
    #[test]
    fn scopes_current_folder_apis_by_cwd() {
        let dir = TempDir::new("flat");
        let project_a = dir.join("project-a");
        let project_b = dir.join("project-b");
        fs::create_dir_all(&project_a).expect("mkdir");
        fs::create_dir_all(&project_b).expect("mkdir");
        let session_a = create_persisted_session(&project_a, dir.path(), "from A");
        set_mtime(&session_a, 10);
        let session_b = create_persisted_session(&project_b, dir.path(), "from B");

        let current_a = SessionManager::list(
            project_a.to_str().expect("UTF-8"),
            Some(dir.path()),
            None,
            None,
        )
        .expect("list");
        let current_paths: Vec<PathBuf> = current_a
            .iter()
            .map(|session| session.path.clone())
            .collect();
        assert_eq!(current_paths, std::slice::from_ref(&session_a));
        let all = SessionManager::list_all(Some(dir.path()), None, None).expect("list all");
        let mut paths: Vec<PathBuf> = all.into_iter().map(|session| session.path).collect();
        paths.sort();
        let mut expected = vec![session_a.clone(), session_b];
        expected.sort();
        assert_eq!(paths, expected);

        let continued =
            SessionManager::continue_recent(project_a.to_str().expect("UTF-8"), Some(dir.path()))
                .expect("continue");
        assert_eq!(continued.session_file(), Some(session_a.as_path()));
        assert_eq!(
            text_of(&continued.build_session_context().messages[0]),
            "from A"
        );
    }

    /// "rejects a cancelled session listing"
    #[test]
    fn rejects_a_cancelled_listing() {
        let dir = TempDir::new("flat-cancel");
        let project_a = dir.join("project-a");
        let project_b = dir.join("project-b");
        create_persisted_session(&project_a, dir.path(), "from A");
        create_persisted_session(&project_b, dir.path(), "from B");
        let cancel = AtomicBool::new(false);
        let mut abort = |_loaded: usize, _total: usize, partial: Option<&[_]>| {
            if partial.is_some() {
                cancel.store(true, std::sync::atomic::Ordering::SeqCst);
            }
        };
        let listing = SessionManager::list_all(Some(dir.path()), Some(&mut abort), Some(&cancel));
        assert!(matches!(listing, Err(SessionError::Aborted)));
        let again = SessionManager::list_all(None, None, Some(&cancel));
        assert!(matches!(again, Err(SessionError::Aborted)));
    }
}

mod corrupted_files {
    use super::*;

    /// "truncates and rewrites empty file with valid header"
    #[test]
    fn rewrites_empty_file_with_header() {
        let dir = TempDir::new("corrupt-empty");
        let file = dir.join("empty.jsonl");
        fs::write(&file, "").expect("write");
        let session = SessionManager::open(&file, Some(dir.path()), None).expect("open");
        assert!(!session.session_id().is_empty());
        let content = fs::read_to_string(&file).expect("read");
        let lines: Vec<&str> = content
            .trim()
            .split('\n')
            .filter(|line| !line.is_empty())
            .collect();
        assert_eq!(lines.len(), 1);
        let header: serde_json::Value = serde_json::from_str(lines[0]).expect("JSON");
        assert_eq!(header["type"], "session");
        assert_eq!(header["id"], session.session_id());
    }

    /// "throws and preserves non-empty file without valid header" and
    /// "throws and preserves non-session JSONL files"
    #[test]
    fn rejects_and_preserves_non_session_files() {
        let dir = TempDir::new("corrupt-invalid");
        for (name, content) in [
            (
                "no-header.jsonl",
                "{\"type\":\"message\",\"id\":\"abc\",\"parentId\":\"orphaned\",\"timestamp\":\"2025-01-01T00:00:00Z\",\"message\":{\"role\":\"assistant\",\"content\":\"test\"}}\n",
            ),
            (
                "not-a-session.log",
                "{\"type\":\"event\",\"data\":\"not a session\"}\n",
            ),
        ] {
            let file = dir.join(name);
            fs::write(&file, content).expect("write");
            let error =
                SessionManager::open(&file, Some(dir.path()), None).expect_err("not a session");
            assert_eq!(
                error.to_string(),
                format!(
                    "Session file is not a valid bake session: {}",
                    file.display()
                )
            );
            assert_eq!(fs::read_to_string(&file).expect("read"), content);
        }
    }

    /// "preserves explicit session file path when recovering from corrupted
    /// file" and "subsequent loads of initialized empty file work correctly"
    #[test]
    fn keeps_the_explicit_path_and_reloads() {
        let dir = TempDir::new("corrupt-path");
        let file = dir.join("my-session.jsonl");
        fs::write(&file, "").expect("write");
        let first = SessionManager::open(&file, Some(dir.path()), None).expect("open");
        assert_eq!(first.session_file(), Some(file.as_path()));
        let second = SessionManager::open(&file, Some(dir.path()), None).expect("reopen");
        assert_eq!(second.session_id(), first.session_id());
        assert!(second.header().is_some());
    }
}

mod file_creation {
    use super::*;

    /// "does not create a file for a session with only setup entries"
    #[test]
    fn no_file_for_setup_entries() {
        let dir = TempDir::new("create-setup");
        let mut session =
            SessionManager::create(dir.str(), Some(dir.path()), NewSessionOptions::default())
                .expect("create");
        session
            .append_model_change("anthropic", "claude-sonnet-4-5")
            .expect("append");
        session.append_thinking_level_change("off").expect("append");
        assert!(!session.session_file().expect("file").exists());
    }

    /// "creates the file when the first user message is appended" (#10000)
    #[test]
    fn creates_file_at_first_user_message() {
        let dir = TempDir::new("create-user");
        let mut session =
            SessionManager::create(dir.str(), Some(dir.path()), NewSessionOptions::default())
                .expect("create");
        session
            .append_model_change("anthropic", "claude-sonnet-4-5")
            .expect("append");
        session
            .append_message(user_msg("first question"))
            .expect("append");
        let file = session.session_file().expect("file").to_path_buf();
        assert_eq!(
            read_session_file_roles(&file),
            ["session", "model_change", "user"]
        );
        let reopened = SessionManager::open(&file, Some(dir.path()), None).expect("open");
        assert_eq!(reopened.build_session_context().messages.len(), 1);
    }

    /// "appends later entries to the file without rewriting earlier ones"
    #[test]
    fn appends_later_entries() {
        let dir = TempDir::new("create-append");
        let mut session =
            SessionManager::create(dir.str(), Some(dir.path()), NewSessionOptions::default())
                .expect("create");
        session
            .append_message(user_msg("first question"))
            .expect("append");
        let file = session.session_file().expect("file").to_path_buf();
        let first_bytes = fs::read(&file).expect("read");
        session
            .append_custom_entry("preset-state", Some(json!({"name": "plan"})))
            .expect("append");
        session
            .append_message(assistant_msg("first answer"))
            .expect("append");
        assert_eq!(
            read_session_file_roles(&file),
            ["session", "user", "custom", "assistant"]
        );
        assert!(fs::read(&file).expect("read").starts_with(&first_bytes));
    }
}
