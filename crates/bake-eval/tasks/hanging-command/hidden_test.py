"""Run in the workspace: job files with repeated ids finish, the exit status
is right, and the queue, retries, and deadline behave as before."""

import os
import subprocess
import sys
import tempfile
import threading
import time

sys.path.insert(0, os.getcwd())

from jobq import Job, JobQueue, RetryPolicy  # noqa: E402
from jobq.batch import describe_result, parse, run_batch  # noqa: E402


def within(seconds, what, fn):
    """Runs fn in a thread; a hang fails the whole test at once."""
    outcome = []

    def run():
        try:
            fn()
            outcome.append(None)
        except BaseException as error:  # reported below
            outcome.append(error)

    thread = threading.Thread(target=run, daemon=True)
    thread.start()
    thread.join(seconds)
    if thread.is_alive():
        print(f"{what}: still waiting after {seconds} s")
        sys.stdout.flush()
        os._exit(1)
    if outcome[0] is not None:
        print(f"{what}: {outcome[0]!r}")
        sys.stdout.flush()
        os._exit(1)


def repeated_ids_finish():
    warnings = []
    text = "a square 2\nb add 1 2\na square 3\nc fail x\nb echo again\nd flaky hidden-d 1 retries=1\n"
    results = run_batch(parse(text), workers=2, warn=warnings.append)
    assert [(e.id, j.state, describe_result(j)) for e, j in results] == [
        ("a", "done", "4"), ("b", "done", "3"), ("c", "failed", "ValueError: x"), ("d", "done", "ok after 2"),
    ], results
    assert warnings == ["line 3: duplicate job id a, skipped", "line 5: duplicate job id b, skipped"], warnings
    assert all(job.finished for _, job in results)


def only_repeats_and_empty():
    assert run_batch(parse("# nothing\n"), workers=1) == []
    results = run_batch(parse("x square 1\nx square 1\nx square 1\n"), workers=1, warn=lambda m: None)
    assert [describe_result(j) for _, j in results] == ["1"]


def shared_queue_waits_for_its_own_jobs():
    gate = threading.Event()
    with JobQueue(workers=2) as queue:
        other = queue.submit(gate.wait)
        results = run_batch(parse("p pause 0.3\nq square 5\np square 1\n"), queue=queue, warn=lambda m: None)
        assert [(j.state, describe_result(j)) for _, j in results] == [("done", "slept 0.3"), ("done", "25")]
        assert other.state == "running"
        gate.set()
        queue.join()


def queue_still_works():
    with JobQueue(workers=3) as jobs:
        flaky = [jobs.submit(pow, n, 2) for n in range(10)]
        bad = jobs.submit(int, "nope")
        jobs.join()
        assert [job.result for job in flaky] == [n * n for n in range(10)]
        assert bad.state == "failed"
    job = Job(int, ("x",), retry=RetryPolicy(attempts=2))
    job.run()
    assert job.state == "retrying" and job.wait(timeout=0.05) is False


within(8, "run_batch with repeated ids", repeated_ids_finish)
within(8, "run_batch with only repeats", only_repeats_and_empty)
within(8, "run_batch on a shared queue", shared_queue_waits_for_its_own_jobs)
within(8, "the queue", queue_still_works)


def cli(*args, limit=15):
    started = time.monotonic()
    ran = subprocess.run([sys.executable, "-m", "jobq.cli", *args], capture_output=True, text=True, timeout=limit)
    return ran, time.monotonic() - started


with tempfile.TemporaryDirectory() as scratch:
    def job_file(name, text):
        path = os.path.join(scratch, name)
        with open(path, "w", encoding="utf-8") as handle:
            handle.write(text)
        return path

    cases = [
        ("ok.jobs", "a square 3\na square 4\nb echo hi\n", 0, "a            done      9\nb            done      hi\n2 done\n",
         "warning: line 2: duplicate job id a, skipped\n"),
        ("bad.jobs", "a fail boom\nb add 2 2\nb add 3 3\n", 1,
         "a            failed    ValueError: boom\nb            done      4\n1 done, 1 failed\n",
         "warning: line 3: duplicate job id b, skipped\n"),
        ("allbad.jobs", "a fail one\nb fail two\n", 1,
         "a            failed    ValueError: one\nb            failed    ValueError: two\n2 failed\n", ""),
    ]
    for name, text, code, stdout, stderr in cases:
        ran, _ = cli(job_file(name, text))
        got = (ran.returncode, ran.stdout, ran.stderr)
        assert got == (code, stdout, stderr), (name, got)
    ran, _ = cli(job_file("parse.jobs", "a\n"))
    assert ran.returncode == 2, (ran.returncode, ran.stderr)
    ran, took = cli("--deadline", "1", job_file("slow.jobs", "a pause 30\n"))
    assert ran.returncode == 3 and took < 10, (ran.returncode, took, ran.stderr)

print("hidden tests passed")
