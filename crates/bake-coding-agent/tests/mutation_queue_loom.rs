//! Compile the production queue with Loom's synchronization, not a second
//! implementation. Keys are fixed; native tests own filesystem resolution.

mod sync {
    pub(crate) use loom::sync::{Arc, Condvar, Mutex, MutexGuard};
    pub(crate) use std::sync::PoisonError;
}

#[path = "../src/tools/file_mutation_queue/queue.rs"]
mod queue;

use loom::sync::atomic::{AtomicUsize, Ordering};
use loom::sync::{Arc, Mutex};
use loom::thread;
use std::path::PathBuf;

fn model(test: impl Fn() + Sync + Send + 'static) {
    let mut model = loom::model::Builder::new();
    model.preemption_bound = Some(2);
    model.max_branches = 1000;
    model.max_permutations = None;
    model.max_duration = None;
    model.check(test);
}

#[test]
fn same_key_excludes_overlap_and_follows_registration_order() {
    model(|| {
        let queue = Arc::new(queue::Queue::new());
        let active = Arc::new(AtomicUsize::new(0));
        let admitted = Arc::new(Mutex::new(Vec::new()));
        let completed = Arc::new(Mutex::new(Vec::new()));
        let mut workers = Vec::new();
        for index in 0..3 {
            let queue = Arc::clone(&queue);
            let active = Arc::clone(&active);
            let admitted = Arc::clone(&admitted);
            let completed = Arc::clone(&completed);
            workers.push(thread::spawn(move || {
                queue
                    .run(
                        || {
                            admitted.lock().unwrap().push(index);
                            Ok(PathBuf::from("same"))
                        },
                        || {
                            assert_eq!(active.fetch_add(1, Ordering::SeqCst), 0);
                            thread::yield_now();
                            completed.lock().unwrap().push(index);
                            assert_eq!(active.fetch_sub(1, Ordering::SeqCst), 1);
                            Ok(())
                        },
                    )
                    .unwrap();
            }));
        }
        for worker in workers {
            worker.join().unwrap();
        }
        assert_eq!(*admitted.lock().unwrap(), *completed.lock().unwrap());
        assert!(queue.is_empty());
    });
}

#[test]
fn completed_tails_do_not_remove_new_registrations() {
    model(|| {
        let queue = Arc::new(queue::Queue::new());
        let active = Arc::new(AtomicUsize::new(0));
        let mut workers = Vec::new();
        for _ in 0..2 {
            let queue = Arc::clone(&queue);
            let active = Arc::clone(&active);
            workers.push(thread::spawn(move || {
                for _ in 0..2 {
                    queue
                        .run(
                            || Ok(PathBuf::from("reuse")),
                            || {
                                assert_eq!(active.fetch_add(1, Ordering::SeqCst), 0);
                                thread::yield_now();
                                assert_eq!(active.fetch_sub(1, Ordering::SeqCst), 1);
                                Ok(())
                            },
                        )
                        .unwrap();
                }
            }));
        }
        for worker in workers {
            worker.join().unwrap();
        }
        assert!(queue.is_empty());
    });
}

#[test]
fn different_keys_progress_while_another_callback_waits() {
    model(|| {
        let queue = Arc::new(queue::Queue::new());
        let (started, running) = loom::sync::mpsc::channel();
        let (release, wait) = loom::sync::mpsc::channel();
        let first_queue = Arc::clone(&queue);
        let first = thread::spawn(move || {
            first_queue
                .run(
                    || Ok(PathBuf::from("first")),
                    || {
                        started.send(()).unwrap();
                        wait.recv().unwrap();
                        Ok(())
                    },
                )
                .unwrap()
        });
        running.recv().unwrap();
        queue
            .run(
                || Ok(PathBuf::from("second")),
                || {
                    release.send(()).unwrap();
                    Ok(())
                },
            )
            .unwrap();
        first.join().unwrap();
        assert!(queue.is_empty());
    });
}
