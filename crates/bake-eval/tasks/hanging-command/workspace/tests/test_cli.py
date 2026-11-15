import os
import subprocess
import sys
import unittest

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))


def run_cli(*args):
    # The nightly cron job starts the CLI in its own session; so do we.
    return subprocess.run(
        [sys.executable, "-m", "jobq.cli", *args],
        cwd=ROOT, capture_output=True, text=True, start_new_session=True,
    )


class CliTest(unittest.TestCase):
    def test_a_clean_file(self):
        ran = run_cli("tests/data/clean.jobs")
        self.assertEqual(ran.returncode, 0, ran.stderr)
        self.assertEqual(ran.stdout.splitlines()[-1], "3 done")

    def test_a_broken_file(self):
        ran = run_cli("tests/data/broken.jobs")
        self.assertEqual((ran.returncode, ran.stderr), (2, "error: tests/data/broken.jobs: line 2: unknown task 'explode'\n"))

    def test_the_nightly_jobs(self):
        ran = run_cli("examples/nightly.jobs")
        self.assertEqual(ran.stderr, "warning: line 8: duplicate job id square-7, skipped\n")
        self.assertEqual(
            ran.stdout,
            "backup       done      backed up 3 volumes\n"
            "square-7     done      49\n"
            "sync         done      ok after 3\n"
            "report       failed    ValueError: no data for yesterday\n"
            "totals       done      15\n"
            "rest         done      slept 0.1\n"
            "5 done, 1 failed\n",
        )
        self.assertEqual(ran.returncode, 1)


if __name__ == "__main__":
    unittest.main()
