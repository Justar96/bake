//! Pi `test/file-mutation-queue.test.ts`, with channels in place of delays.
//! The edit/write cases exercise synchronous filesystem callbacks; tool schemas
//! and dispatch are not part of this module.

use super::*;
use std::fs;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::thread;
use std::time::Duration;

const WAIT: Duration = Duration::from_secs(30);

struct TempDir(PathBuf);

impl TempDir {
    fn new() -> Self {
        let mut bytes = [0; 16];
        getrandom::getrandom(&mut bytes).unwrap();
        let id: String = bytes.iter().map(|byte| format!("{byte:02x}")).collect();
        let dir = std::env::temp_dir().join(format!("bake-mutation-test-{id}"));
        fs::create_dir(&dir).unwrap();
        Self(fs::canonicalize(dir).unwrap())
    }
    fn file(&self, name: &str) -> PathBuf {
        self.0.join(name)
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.0).unwrap();
    }
}

#[test]
fn same_file_callbacks_run_in_registration_order() {
    let queue = queue::Queue::new();
    let order = Mutex::new(Vec::new());
    let (release, blocked) = mpsc::channel();
    let (registered, registrations) = mpsc::channel();
    let queue_ref = &queue;
    let order_ref = &order;
    let registered_ref = &registered;
    thread::scope(|scope| {
        let first = scope.spawn(move || {
            queue_ref.run(
                || {
                    registered_ref.send(0).unwrap();
                    Ok(PathBuf::from("same"))
                },
                || {
                    order_ref.lock().unwrap().push("first:start");
                    blocked.recv_timeout(WAIT).unwrap();
                    order_ref.lock().unwrap().push("first:end");
                    Ok(())
                },
            )
        });
        assert_eq!(registrations.recv_timeout(WAIT).unwrap(), 0);
        let second = scope.spawn(|| {
            queue.run(
                || {
                    registered.send(1).unwrap();
                    Ok(PathBuf::from("same"))
                },
                || {
                    order.lock().unwrap().push("second");
                    Ok(())
                },
            )
        });
        assert_eq!(registrations.recv_timeout(WAIT).unwrap(), 1);
        let third = scope.spawn(|| {
            queue.run(
                || {
                    registered.send(2).unwrap();
                    Ok(PathBuf::from("same"))
                },
                || {
                    order.lock().unwrap().push("third");
                    Ok(())
                },
            )
        });
        assert_eq!(registrations.recv_timeout(WAIT).unwrap(), 2);
        release.send(()).unwrap();
        first.join().unwrap().unwrap();
        second.join().unwrap().unwrap();
        third.join().unwrap().unwrap();
    });
    assert_eq!(
        *order.lock().unwrap(),
        ["first:start", "first:end", "second", "third"]
    );
    assert!(queue.is_empty());
}

#[test]
fn different_files_proceed_while_another_callback_is_blocked() {
    let root = TempDir::new();
    let first_path = root.file("a");
    let second_path = root.file("b");
    let (started, first_started) = mpsc::channel();
    let (second_done, done) = mpsc::channel();
    thread::scope(|scope| {
        let first = scope.spawn(move || {
            with_file_mutation_queue(&first_path, || {
                started.send(()).unwrap();
                done.recv_timeout(WAIT).unwrap();
                fs::write(&first_path, "a")
            })
        });
        first_started.recv_timeout(WAIT).unwrap();
        let second = scope.spawn(move || {
            with_file_mutation_queue(&second_path, || {
                fs::write(&second_path, "b")?;
                second_done.send(()).unwrap();
                Ok(())
            })
        });
        first.join().unwrap().unwrap();
        second.join().unwrap().unwrap();
    });
    assert_eq!(fs::read(root.file("a")).unwrap(), b"a");
    assert_eq!(fs::read(root.file("b")).unwrap(), b"b");
}

fn ordered_edits(first_path: &Path, second_path: &Path) {
    let queue = queue::Queue::new();
    let (started, running) = mpsc::channel();
    let (registered, next_registered) = mpsc::channel();
    let queue_ref = &queue;
    thread::scope(|scope| {
        let first = scope.spawn(move || {
            queue_ref.run(
                || mutation_key(first_path),
                || {
                    let text = fs::read_to_string(first_path)?;
                    started.send(()).unwrap();
                    next_registered.recv_timeout(WAIT).unwrap();
                    fs::write(first_path, text.replace("alpha", "ALPHA"))
                },
            )
        });
        running.recv_timeout(WAIT).unwrap();
        let second = scope.spawn(|| {
            queue.run(
                || {
                    let key = mutation_key(second_path)?;
                    registered.send(()).unwrap();
                    Ok(key)
                },
                || {
                    let text = fs::read_to_string(second_path)?;
                    fs::write(second_path, text.replace("beta", "BETA"))
                },
            )
        });
        first.join().unwrap().unwrap();
        second.join().unwrap().unwrap();
    });
    assert_eq!(
        fs::read_to_string(first_path).unwrap(),
        "ALPHA\nBETA\ngamma\n"
    );
    assert!(queue.is_empty());
}

#[test]
fn both_parallel_edits_survive() {
    let root = TempDir::new();
    let path = root.file("parallel-edit.txt");
    fs::write(&path, "alpha\nbeta\ngamma\n").unwrap();
    ordered_edits(&path, &path);
}

#[test]
fn lexical_aliases_of_existing_files_share_the_queue() {
    let root = TempDir::new();
    let path = root.file("target.txt");
    fs::write(&path, "alpha\nbeta\ngamma\n").unwrap();
    fs::create_dir(root.file("child")).unwrap();
    ordered_edits(&path, &root.file("child/.././target.txt"));
}

#[cfg(unix)]
#[test]
fn existing_symlink_aliases_share_the_queue() {
    let root = TempDir::new();
    let path = root.file("target.txt");
    let alias = root.file("alias.txt");
    fs::write(&path, "alpha\nbeta\ngamma\n").unwrap();
    std::os::unix::fs::symlink(&path, &alias).unwrap();
    ordered_edits(&path, &alias);
}

#[test]
fn write_waits_for_edit_and_retains_the_replacement() {
    let root = TempDir::new();
    let path = root.file("mixed.txt");
    fs::write(&path, "original\n").unwrap();
    let queue = queue::Queue::new();
    let (started, running) = mpsc::channel();
    let (registered, next_registered) = mpsc::channel();
    let queue_ref = &queue;
    let path_ref = &path;
    thread::scope(|scope| {
        let first = scope.spawn(move || {
            queue_ref.run(
                || mutation_key(path_ref),
                || {
                    let text = fs::read_to_string(path_ref)?;
                    started.send(()).unwrap();
                    next_registered.recv_timeout(WAIT).unwrap();
                    fs::write(path_ref, text.replace("original", "edited"))
                },
            )
        });
        running.recv_timeout(WAIT).unwrap();
        let second = scope.spawn(|| {
            queue.run(
                || {
                    let key = mutation_key(&path)?;
                    registered.send(()).unwrap();
                    Ok(key)
                },
                || fs::write(&path, "replacement\n"),
            )
        });
        first.join().unwrap().unwrap();
        second.join().unwrap().unwrap();
    });
    assert_eq!(fs::read_to_string(&path).unwrap(), "replacement\n");
    assert!(queue.is_empty());
}

fn aborted_mutation_keeps_ownership(edit: bool) {
    let root = TempDir::new();
    let path = root.file("abort.txt");
    fs::write(&path, "alpha\nbeta\n").unwrap();
    let queue = queue::Queue::new();
    let queue_ref = &queue;
    let path_ref = &path;
    let aborted = AtomicBool::new(false);
    let settled = AtomicBool::new(false);
    let aborted_ref = &aborted;
    let settled_ref = &settled;
    let (started, running) = mpsc::channel();
    let (registered, next_registered) = mpsc::channel();
    thread::scope(|scope| {
        let first = scope.spawn(move || {
            queue_ref.run(
                || mutation_key(path_ref),
                || {
                    let content = if edit {
                        fs::read_to_string(path_ref)?.replace("alpha", "ALPHA")
                    } else {
                        "first\n".to_owned()
                    };
                    started.send(()).unwrap();
                    next_registered.recv_timeout(WAIT).unwrap();
                    fs::write(path_ref, content)?;
                    settled_ref.store(true, Ordering::SeqCst);
                    assert!(aborted_ref.load(Ordering::SeqCst));
                    Err::<(), _>(io::Error::new(
                        io::ErrorKind::Interrupted,
                        "Operation aborted",
                    ))
                },
            )
        });
        running.recv_timeout(WAIT).unwrap();
        aborted.store(true, Ordering::SeqCst);
        let second = scope.spawn(|| {
            queue.run(
                || {
                    let key = mutation_key(&path)?;
                    registered.send(()).unwrap();
                    Ok(key)
                },
                || {
                    assert!(settled.load(Ordering::SeqCst));
                    if edit {
                        fs::write(&path, fs::read_to_string(&path)?.replace("beta", "BETA"))
                    } else {
                        fs::write(&path, "second\n")
                    }
                },
            )
        });
        assert_eq!(
            first.join().unwrap().unwrap_err().kind(),
            io::ErrorKind::Interrupted
        );
        second.join().unwrap().unwrap();
    });
    assert_eq!(
        fs::read_to_string(&path).unwrap(),
        if edit { "ALPHA\nBETA\n" } else { "second\n" }
    );
    assert!(queue.is_empty());
}

#[test]
fn an_aborted_write_stays_queued_until_its_io_settles() {
    aborted_mutation_keeps_ownership(false);
}

#[test]
fn an_aborted_edit_stays_queued_until_its_io_settles() {
    aborted_mutation_keeps_ownership(true);
}

#[test]
fn missing_paths_use_absolute_lexical_keys_without_creating_files() {
    let root = TempDir::new();
    let path = root.file("missing");
    assert_eq!(
        mutation_key(&root.file("not-there/../missing")).unwrap(),
        normalize_key(path.clone())
    );
    fs::write(root.file("file"), "x").unwrap();
    assert_eq!(
        mutation_key(&root.file("file/child")).unwrap(),
        normalize_key(root.file("file/child"))
    );
    assert_eq!(
        mutation_key(Path::new("relative/../missing")).unwrap(),
        normalize_key(std::env::current_dir().unwrap().join("missing"))
    );
    assert!(!path.exists());
}

#[test]
fn creating_a_file_does_not_change_its_key() {
    let root = TempDir::new();
    let path = root.file("new.txt");
    let before = mutation_key(&path).unwrap();
    fs::write(&path, "created").unwrap();
    assert_eq!(mutation_key(&path).unwrap(), before);
}

#[cfg(windows)]
#[test]
fn windows_verbatim_keys_match_regular_drive_and_unc_paths() {
    for (verbatim, ordinary) in [
        (r"\\?\C:\work\file", r"C:\work\file"),
        (r"\\?\UNC\server\share\file", r"\\server\share\file"),
    ] {
        assert_eq!(
            normalize_key(PathBuf::from(verbatim)),
            PathBuf::from(ordinary)
        );
    }
}

#[test]
fn invalid_paths_do_not_run_the_callback_and_errors_do_not_poison_registration() {
    assert!(
        with_file_mutation_queue::<()>(Path::new("bad\0path"), || panic!(
            "invalid path must not run"
        ))
        .is_err()
    );
    let root = TempDir::new();
    with_file_mutation_queue(&root.file("good"), || fs::write(root.file("good"), "ok")).unwrap();
    assert_eq!(fs::read(root.file("good")).unwrap(), b"ok");
}

#[test]
fn callback_errors_and_panics_release_the_queue() {
    let root = TempDir::new();
    let path = root.file("file");
    let error = with_file_mutation_queue(&path, || {
        Err::<(), _>(io::Error::new(io::ErrorKind::PermissionDenied, "denied"))
    })
    .unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
    assert_eq!(error.to_string(), "denied");
    let unwind = std::panic::catch_unwind(|| {
        with_file_mutation_queue::<()>(&path, || panic!("callback failed"))
    });
    assert!(unwind.is_err());
    with_file_mutation_queue(&path, || fs::write(&path, "recovered")).unwrap();
    assert_eq!(fs::read(&path).unwrap(), b"recovered");
}

#[test]
fn successful_return_values_survive_and_registration_storage_is_reclaimed() {
    let queue = queue::Queue::new();
    for index in 0..1000 {
        assert_eq!(
            queue
                .run(|| Ok(PathBuf::from(index.to_string())), || Ok(index))
                .unwrap(),
            index
        );
        assert!(queue.is_empty());
    }
}

// Linux filesystems allow raw filename bytes; macOS can reject them with EILSEQ.
#[cfg(target_os = "linux")]
#[test]
fn path_keys_preserve_non_utf8_bytes() {
    use std::os::unix::ffi::OsStringExt;
    let root = TempDir::new();
    let path = root
        .0
        .join(std::ffi::OsString::from_vec(b"file-\xff".to_vec()));
    with_file_mutation_queue(&path, || fs::write(&path, "data")).unwrap();
    assert_eq!(
        mutation_key(&path).unwrap(),
        fs::canonicalize(&path).unwrap()
    );
}

#[test]
fn concurrent_registration_and_release_do_not_lose_writes() {
    let root = TempDir::new();
    let path = Arc::new(root.file("count"));
    fs::write(path.as_ref(), "0").unwrap();
    thread::scope(|scope| {
        for _ in 0..8 {
            let path = Arc::clone(&path);
            scope.spawn(move || {
                for _ in 0..32 {
                    with_file_mutation_queue(&path, || {
                        let count: usize = fs::read_to_string(path.as_ref())?.parse().unwrap();
                        thread::yield_now();
                        fs::write(path.as_ref(), (count + 1).to_string())
                    })
                    .unwrap();
                }
            });
        }
    });
    assert_eq!(fs::read_to_string(path.as_ref()).unwrap(), "256");
}

#[test]
fn generated_read_modify_write_sequences_match_pi() {
    let oracle: serde_json::Value = serde_json::from_str(include_str!(
        "../../../tests/fixtures/pi-mutation-queue/golden.json"
    ))
    .unwrap();
    let root = TempDir::new();
    fs::create_dir(root.file("folder")).unwrap();
    for case in oracle["cases"].as_array().unwrap() {
        for name in ["a", "b", "c"] {
            fs::write(root.file(name), "").unwrap();
        }
        let queue = queue::Queue::new();
        thread::scope(|scope| {
            let (registered, registrations) = mpsc::channel();
            let mut workers = Vec::new();
            for (index, operation) in case["operations"].as_array().unwrap().iter().enumerate() {
                let path = root.file(operation["path"].as_str().unwrap());
                let text = operation["text"].as_str().unwrap();
                let registered = registered.clone();
                let queue = &queue;
                workers.push(scope.spawn(move || {
                    queue.run(
                        || {
                            let key = mutation_key(&path)?;
                            registered.send(index).unwrap();
                            Ok(key)
                        },
                        || {
                            let before = fs::read_to_string(&path)?;
                            thread::yield_now();
                            fs::write(&path, before + text)
                        },
                    )
                }));
                // Match Pi's call order without imposing an execution order
                // on callbacks for different files.
                assert_eq!(registrations.recv_timeout(WAIT).unwrap(), index);
            }
            for worker in workers {
                worker.join().unwrap().unwrap();
            }
        });
        for (name, expected) in case["expected"].as_object().unwrap() {
            assert_eq!(
                fs::read_to_string(root.file(name)).unwrap(),
                expected.as_str().unwrap()
            );
        }
        assert!(queue.is_empty());
    }
}
