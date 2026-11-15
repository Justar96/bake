import threading
import unittest

from jobq import JobQueue, QueueClosed
from jobq.report import describe


def fail(message):
    raise ValueError(message)


class QueueTest(unittest.TestCase):
    def test_jobs_run_and_join_waits_for_them(self):
        with JobQueue(workers=3) as jobs:
            submitted = [jobs.submit(pow, n, 2) for n in range(20)]
            jobs.join()
            self.assertEqual([job.result for job in submitted], [n * n for n in range(20)])
            self.assertEqual(jobs.summary(), {"done": 20})

    def test_a_failed_job_does_not_stop_the_others(self):
        with JobQueue(workers=2) as jobs:
            good = jobs.submit(len, "abc")
            bad = jobs.submit(fail, "no")
            jobs.join()
            self.assertEqual((good.state, good.result), ("done", 3))
            self.assertEqual((bad.state, str(bad.error)), ("failed", "no"))
            self.assertEqual(describe(jobs.summary()), "1 done, 1 failed")

    def test_a_cancelled_job_never_runs(self):
        gate = threading.Event()
        ran = []
        with JobQueue(workers=1) as jobs:
            jobs.submit(gate.wait)
            later = jobs.submit(ran.append, "later")
            self.assertTrue(later.cancel())
            gate.set()
            jobs.join()
        self.assertEqual((later.state, ran), ("cancelled", []))

    def test_a_closed_queue_refuses_jobs(self):
        jobs = JobQueue(workers=1)
        jobs.close()
        with self.assertRaises(QueueClosed):
            jobs.submit(print)

    def test_an_empty_summary(self):
        with JobQueue(workers=1) as jobs:
            self.assertEqual(describe(jobs.summary()), "no jobs")


if __name__ == "__main__":
    unittest.main()
