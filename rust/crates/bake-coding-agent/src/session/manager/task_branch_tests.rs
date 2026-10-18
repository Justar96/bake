//! Task-only forks must retain acknowledgements without changing Pi's ordinary
//! lazy session creation. Each test owns its paths and writer lifetimes.

use super::*;
use crate::session::GroupEntry;
use crate::task_documents::{self, DocumentKind, DocumentRef, DocumentState, DocumentUpdate};
use crate::test_support::TempDir;

fn create(dir: &TempDir) -> SessionManager {
    SessionManager::create(
        &dir.path().to_string_lossy(),
        Some(&dir.join("sessions")),
        NewSessionOptions::default(),
    )
    .unwrap()
}

fn update(body: &str) -> DocumentUpdate {
    DocumentUpdate {
        id: "goal".into(),
        kind: DocumentKind::Goal,
        body: body.into(),
        state: DocumentState::Active,
    }
}

fn document(session: &mut SessionManager) -> DocumentRef {
    task_documents::revise(session, None, update("finish the check")).unwrap()
}

#[test]
fn task_only_forks_are_reopenable_at_acknowledgement() {
    for copied in [false, true] {
        let dir = TempDir::new("task-fork-reopen");
        let mut source = create(&dir);
        let expected = document(&mut source);
        let source_file = source.session_file().unwrap().to_path_buf();
        let source_id = source.session_id().to_owned();
        let leaf = source.leaf_id().unwrap().to_owned();
        let context = source.build_session_context();
        let fork_file = if copied {
            let fork = source.branched_copy(Some(&leaf)).unwrap();
            let path = fork.session_file().unwrap().to_path_buf();
            assert!(path.exists(), "successful copied task fork must exist");
            assert_eq!(source.session_id(), source_id);
            assert_eq!(source.session_file(), Some(source_file.as_path()));
            assert!(matches!(
                SessionManager::open(&source_file, None, None),
                Err(SessionError::Locked(_))
            ));
            drop(fork);
            path
        } else {
            source.create_branched_session(&leaf).unwrap().unwrap()
        };
        assert!(fork_file.exists(), "successful task fork must exist");
        assert_ne!(fork_file, source_file);
        drop(source);
        let opened = SessionManager::open(&fork_file, None, None).unwrap();
        assert_eq!(
            task_documents::project(&opened).unwrap()["goal"].reference(),
            expected
        );
        assert_eq!(opened.build_session_context(), context);
        assert_eq!(
            opened.header().unwrap().parent_session(),
            source_file.to_str()
        );
        let original = SessionManager::open(&source_file, None, None).unwrap();
        assert_eq!(
            task_documents::project(&original).unwrap()["goal"].reference(),
            expected
        );
    }
}

#[test]
fn generic_custom_ancestry_and_tasks_on_other_branches_stay_lazy() {
    let dir = TempDir::new("task-fork-lazy");
    let mut source = create(&dir);
    let generic = source
        .append_durable_group(vec![GroupEntry::Custom {
            custom_type: "another.extension".into(),
            data: Some(Value::Null),
        }])
        .unwrap()
        .remove(0);
    assert!(source.session_file().unwrap().exists());
    let fork = source.branched_copy(Some(&generic)).unwrap();
    assert!(!fork.session_file().unwrap().exists());
    document(&mut source);
    let fork = source.branched_copy(Some(&generic)).unwrap();
    assert!(!fork.session_file().unwrap().exists());
    assert!(task_documents::project(&fork).unwrap().is_empty());
}

#[test]
fn a_failed_task_fork_does_not_acknowledge_and_copy_keeps_its_source() {
    for copied in [false, true] {
        let dir = TempDir::new("task-fork-failure");
        let mut source = create(&dir);
        let expected = document(&mut source);
        let source_file = source.session_file().unwrap().to_path_buf();
        let source_id = source.session_id().to_owned();
        let leaf = source.leaf_id().unwrap().to_owned();
        let bytes = fs::read(&source_file).unwrap();
        // A regular file is deterministically unusable as the new directory,
        // including when tests run as root. The source writer remains valid.
        let destination = dir.join("not-a-directory");
        fs::write(&destination, "owned obstruction").unwrap();
        source.session_dir = destination.clone();
        if copied {
            assert!(source.branched_copy(Some(&leaf)).is_err());
            assert_eq!(source.session_id(), source_id);
            assert_eq!(source.session_file(), Some(source_file.as_path()));
            assert_eq!(source.leaf_id(), Some(leaf.as_str()));
            assert_eq!(fs::read(&source_file).unwrap(), bytes);
            task_documents::revise(
                &mut source,
                Some(&expected),
                update("source remains writable"),
            )
            .unwrap();
        } else {
            assert!(source.create_branched_session(&leaf).is_err());
            assert_eq!(fs::read(&source_file).unwrap(), bytes);
        }
        assert_eq!(
            fs::read_to_string(destination).unwrap(),
            "owned obstruction"
        );
        drop(source);
        let opened = SessionManager::open(&source_file, None, None).unwrap();
        assert_eq!(
            task_documents::project(&opened).unwrap()["goal"]
                .snapshot
                .revision,
            if copied { 2 } else { 1 }
        );
    }
}
