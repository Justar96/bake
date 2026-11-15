import io
import os
import unittest

from jobq import JobQueue
from jobq.batch import BatchError, Entry, describe_result, parse, run_batch

HERE = os.path.dirname(os.path.abspath(__file__))


class ParseTest(unittest.TestCase):
    def test_lines_comments_and_retries(self):
        entries = parse("# header\n\na square 4\nb flaky key 1 retries=3  # note\n")
        self.assertEqual(entries, [Entry(3, "a", "square", ("4",), 0), Entry(4, "b", "flaky", ("key", "1"), 3)])

    def test_bad_lines(self):
        for text in ("lonely\n", "a explode\n", "a square 1 retries=x\n"):
            with self.assertRaises(BatchError, msg=text):
                parse(text)


class RunTest(unittest.TestCase):
    def test_a_clean_file(self):
        with open(os.path.join(HERE, "data", "clean.jobs"), encoding="utf-8") as handle:
            results = run_batch(parse(handle.read()))
        self.assertEqual([(e.id, j.state, describe_result(j)) for e, j in results],
                         [("one", "done", "9"), ("two", "done", "6"), ("three", "done", "all good")])

    def test_failures_and_retries(self):
        entries = parse("x fail bad input\ny flaky run-test 1 retries=1\nz flaky run-test-2 5 retries=1\n")
        results = run_batch(entries, workers=3)
        self.assertEqual([describe_result(j) for _, j in results],
                         ["ValueError: bad input", "ok after 2", "ConnectionError: attempt 2 of run-test-2 failed"])

    def test_a_shared_queue_keeps_its_other_work(self):
        with JobQueue(workers=2) as queue:
            other = queue.submit(pow, 2, 10)
            results = run_batch(parse("a add 1 1\n"), queue=queue)
            self.assertEqual(describe_result(results[0][1]), "2")
            queue.join()
            self.assertEqual(other.result, 1024)


if __name__ == "__main__":
    unittest.main()
