//! The registration and completion chain, shared by production and Loom.
//! Resolution is injected so the model can use fixed keys without filesystem I/O.

use std::collections::HashMap;
use std::io;
use std::path::PathBuf;

use super::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};

#[derive(Debug)]
struct Completion {
    done: Mutex<bool>,
    changed: Condvar,
}

impl Completion {
    fn new() -> Self {
        Self {
            done: Mutex::new(false),
            changed: Condvar::new(),
        }
    }

    fn wait(&self) {
        let mut done = lock(&self.done);
        while !*done {
            done = self
                .changed
                .wait(done)
                .unwrap_or_else(PoisonError::into_inner);
        }
    }

    fn finish(&self) {
        *lock(&self.done) = true;
        self.changed.notify_all();
    }
}

#[derive(Debug)]
pub(super) struct Queue {
    tails: Mutex<HashMap<PathBuf, Arc<Completion>>>,
}

impl Queue {
    pub(super) fn new() -> Self {
        Self {
            tails: Mutex::new(HashMap::new()),
        }
    }

    pub(super) fn run<T>(
        &self,
        resolve: impl FnOnce() -> io::Result<PathBuf>,
        operation: impl FnOnce() -> io::Result<T>,
    ) -> io::Result<T> {
        let (guard, previous) = {
            let mut tails = lock(&self.tails);
            // Pi serializes realpath with registration. An alias lookup must
            // not overtake an earlier call while that call resolves its key.
            let key = resolve()?;
            let completion = Arc::new(Completion::new());
            let previous = tails.insert(key.clone(), Arc::clone(&completion));
            (
                Release {
                    queue: self,
                    key,
                    completion,
                },
                previous,
            )
        };
        if let Some(previous) = previous {
            previous.wait();
        }
        let result = operation();
        drop(guard);
        result
    }

    #[cfg(test)]
    pub(super) fn is_empty(&self) -> bool {
        lock(&self.tails).is_empty()
    }
}

struct Release<'a> {
    queue: &'a Queue,
    key: PathBuf,
    completion: Arc<Completion>,
}

impl Drop for Release<'_> {
    fn drop(&mut self) {
        self.completion.finish();
        let mut tails = lock(&self.queue.tails);
        if tails
            .get(&self.key)
            .is_some_and(|tail| Arc::ptr_eq(tail, &self.completion))
        {
            tails.remove(&self.key);
        }
    }
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    // Callbacks run outside these locks. Recovering poison also lets tests
    // unwind a resolver without disabling unrelated later registrations.
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}
