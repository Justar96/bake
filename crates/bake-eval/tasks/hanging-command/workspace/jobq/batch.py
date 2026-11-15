"""Job files and running them.

A job file holds one job a line, `<id> <task> [arg...] [retries=N]`, where
the task is a name in `jobq.tasks.TASKS`; `#` starts a comment. Ids are
unique: a line that repeats an earlier id is skipped with a warning.
"""

import sys
import threading
from dataclasses import dataclass

from jobq import states
from jobq.queue import JobQueue
from jobq.retry import RetryPolicy
from jobq.tasks import TASKS


class BatchError(Exception):
    pass


@dataclass(frozen=True)
class Entry:
    line: int
    id: str
    task: str
    args: tuple
    retries: int = 0


def parse(text):
    """The entries of a job file, in file order, repeated ids included."""
    entries = []
    for number, raw in enumerate(text.splitlines(), 1):
        line = raw.split("#", 1)[0].strip()
        if not line:
            continue
        words = line.split()
        if len(words) < 2:
            raise BatchError(f"line {number}: expected <id> <task> [arg...]")
        job_id, task = words[0], words[1]
        if task not in TASKS:
            raise BatchError(f"line {number}: unknown task {task!r}")
        retries = 0
        args = []
        for word in words[2:]:
            if word.startswith("retries="):
                value = word[len("retries="):]
                if not value.isdigit():
                    raise BatchError(f"line {number}: retries must be a whole number")
                retries = int(value)
            else:
                args.append(word)
        entries.append(Entry(number, job_id, task, tuple(args), retries))
    return entries


class Tally:
    """Counts finished jobs, so a caller can wait for a known number of them."""

    def __init__(self, expected):
        self.expected = expected
        self.count = 0
        self._changed = threading.Condition()

    def done(self, job):
        with self._changed:
            self.count += 1
            self._changed.notify_all()

    def wait(self):
        with self._changed:
            self._changed.wait_for(lambda: self.count >= self.expected)


def _warn(message):
    print(f"warning: {message}", file=sys.stderr)


def run_batch(entries, queue=None, workers=2, warn=_warn):
    """Runs the entries and returns [(entry, job)] in file order, once every
    job is finished. With `queue`, the jobs go to that queue, which may hold
    other work; only these jobs are waited for."""
    own = queue is None
    if own:
        queue = JobQueue(workers=workers)
    try:
        tally = Tally(len(entries))
        seen = set()
        submitted = []
        for entry in entries:
            if entry.id in seen:
                warn(f"line {entry.line}: duplicate job id {entry.id}, skipped")
                continue
            seen.add(entry.id)
            retry = RetryPolicy(attempts=entry.retries + 1, backoff=0.01)
            job = queue.submit(TASKS[entry.task], *entry.args, retry=retry, name=entry.id)
            job.add_done_callback(tally.done)
            submitted.append((entry, job))
        tally.wait()
        return submitted
    finally:
        if own:
            queue.close()


def describe_result(job):
    """The result column of a job's line."""
    if job.state == states.DONE:
        return str(job.result)
    if job.state == states.FAILED:
        return f"{type(job.error).__name__}: {job.error}"
    return "-"
