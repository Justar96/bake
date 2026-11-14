import unittest

from jobq import JobQueue, RetryPolicy


class Flaky:
    """Fails `failures` times, then returns "ok"."""

    def __init__(self, failures, error=ConnectionError):
        self.failures = failures
        self.error = error
        self.calls = 0

    def __call__(self):
        self.calls += 1
        if self.calls <= self.failures:
            raise self.error(f"failure {self.calls}")
        return "ok"


class RetryTest(unittest.TestCase):
    def test_the_policy(self):
        policy = RetryPolicy(attempts=3, retry_on=(ConnectionError,), backoff=0.5)
        self.assertTrue(policy.should_retry(1, ConnectionError()))
        self.assertTrue(policy.should_retry(2, ConnectionError()))
        self.assertFalse(policy.should_retry(3, ConnectionError()))
        self.assertFalse(policy.should_retry(1, KeyError()))
        self.assertEqual([policy.delay(n) for n in (1, 2, 3)], [0.5, 1.0, 2.0])

    def test_a_flaky_job_succeeds_on_a_later_try(self):
        flaky = Flaky(2)
        with JobQueue(workers=2) as jobs:
            job = jobs.submit(flaky, retry=RetryPolicy(attempts=3, backoff=0.01))
            jobs.join()
        self.assertEqual((job.state, job.result, job.attempts), ("done", "ok", 3))

    def test_retries_run_out(self):
        flaky = Flaky(5)
        with JobQueue(workers=2) as jobs:
            job = jobs.submit(flaky, retry=RetryPolicy(attempts=2, backoff=0.01))
            jobs.join()
        self.assertEqual((job.state, str(job.error), flaky.calls), ("failed", "failure 2", 2))

    def test_other_errors_are_not_retried(self):
        flaky = Flaky(1, error=KeyError)
        with JobQueue(workers=1) as jobs:
            job = jobs.submit(flaky, retry=RetryPolicy(attempts=3, retry_on=(ConnectionError,)))
            jobs.join()
        self.assertEqual((job.state, job.attempts), ("failed", 1))


if __name__ == "__main__":
    unittest.main()
