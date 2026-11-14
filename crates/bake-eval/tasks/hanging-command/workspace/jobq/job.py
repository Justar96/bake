"""One unit of work and its outcome."""

import itertools
import threading

from jobq import states
from jobq.retry import NO_RETRY

_ids = itertools.count(1)


class Job:
    def __init__(self, fn, args=(), retry=NO_RETRY, name=None):
        self.id = next(_ids)
        self.name = name or getattr(fn, "__name__", "job")
        self.fn = fn
        self.args = tuple(args)
        self.retry = retry
        self.state = states.PENDING
        self.result = None
        self.error = None
        self.attempts = 0
        self._changed = threading.Condition()
        self._callbacks = []

    def _set(self, state):
        with self._changed:
            self.state = state
            self._changed.notify_all()
            callbacks = list(self._callbacks) if self.finished else []
        for callback in callbacks:
            callback(self)

    def add_done_callback(self, callback):
        """Calls `callback(job)` once the job is finished, at once if it is."""
        with self._changed:
            if not self.finished:
                self._callbacks.append(callback)
                return
        callback(self)

    @property
    def finished(self):
        return self.state in states.FINISHED_STATES

    def run(self):
        """Makes one attempt and records its outcome."""
        self.attempts += 1
        self._set(states.RUNNING)
        try:
            value = self.fn(*self.args)
        except Exception as error:  # a job's failure is its result, not ours
            self.error = error
            if self.retry.should_retry(self.attempts, error):
                self._set(states.RETRYING)
            else:
                self._set(states.FAILED)
        else:
            self.result = value
            self.error = None
            self._set(states.DONE)

    def cancel(self):
        """Cancels the job if no worker has taken it yet."""
        with self._changed:
            if self.state != states.PENDING:
                return False
            self.state = states.CANCELLED
            self._changed.notify_all()
            callbacks = list(self._callbacks)
        for callback in callbacks:
            callback(self)
        return True

    def wait(self, timeout=None):
        """Waits until the job is finished; False if `timeout` ran out first."""
        with self._changed:
            return self._changed.wait_for(lambda: self.finished, timeout)

    def __repr__(self):
        return f"<Job {self.id} {self.name} {self.state}>"
