"""Worker threads that run jobs."""

import queue
import threading
import time

from jobq import states
from jobq.job import Job
from jobq.retry import NO_RETRY


class QueueClosed(Exception):
    pass


class JobQueue:
    def __init__(self, workers=2):
        self._jobs = queue.Queue()
        self._lock = threading.Condition()
        self._unfinished = 0
        self._closed = False
        self._history = []
        self._threads = [threading.Thread(target=self._work, daemon=True) for _ in range(workers)]
        for thread in self._threads:
            thread.start()

    def submit(self, fn, *args, retry=NO_RETRY, name=None):
        job = Job(fn, args, retry=retry, name=name)
        with self._lock:
            if self._closed:
                raise QueueClosed("the queue is closed")
            self._unfinished += 1
            self._history.append(job)
        self._jobs.put(job)
        return job

    def _work(self):
        while True:
            job = self._jobs.get()
            if job is None:
                return
            if job.state == states.CANCELLED:
                self._task_done()
                continue
            job.run()
            if job.state == states.RETRYING:
                # Back to the end of the line; it is still unfinished.
                time.sleep(job.retry.delay(job.attempts))
                self._jobs.put(job)
            elif job.finished:
                self._task_done()

    def _task_done(self):
        with self._lock:
            self._unfinished -= 1
            if self._unfinished == 0:
                self._lock.notify_all()

    def join(self):
        """Waits until every submitted job is finished."""
        with self._lock:
            while self._unfinished:
                self._lock.wait()

    def summary(self):
        """How many jobs are in each state, for the states that have any."""
        with self._lock:
            jobs = list(self._history)
        counts = {}
        for job in jobs:
            counts[job.state] = counts.get(job.state, 0) + 1
        return dict(sorted(counts.items()))

    def close(self):
        """Stops taking jobs, lets the workers finish what is queued, and stops them."""
        with self._lock:
            if self._closed:
                return
            self._closed = True
        for _ in self._threads:
            self._jobs.put(None)
        for thread in self._threads:
            thread.join()

    def __enter__(self):
        return self

    def __exit__(self, *exc):
        self.close()
