import unittest

from jobq import Job, RetryPolicy


def boom():
    raise RuntimeError("boom")


class JobTest(unittest.TestCase):
    def test_a_job_records_its_result(self):
        job = Job(lambda a, b: a + b, (2, 3))
        job.run()
        self.assertEqual((job.state, job.result, job.attempts), ("done", 5, 1))
        self.assertTrue(job.wait())

    def test_a_job_records_its_error(self):
        job = Job(boom)
        job.run()
        self.assertEqual(job.state, "failed")
        self.assertIsInstance(job.error, RuntimeError)
        self.assertTrue(job.wait())

    def test_a_job_that_may_retry_waits_for_its_next_try(self):
        job = Job(boom, retry=RetryPolicy(attempts=2))
        job.run()
        self.assertEqual(job.state, "retrying")
        self.assertFalse(job.wait(timeout=0.05))

    def test_only_a_pending_job_can_be_cancelled(self):
        job = Job(boom)
        self.assertTrue(job.cancel())
        self.assertEqual(job.state, "cancelled")
        self.assertFalse(job.cancel())
        done = Job(int)
        done.run()
        self.assertFalse(done.cancel())


if __name__ == "__main__":
    unittest.main()
