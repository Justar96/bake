"""python3 -m jobq.cli [--workers N] [--deadline SECONDS] FILE

Runs the jobs in a job file (see jobq.batch) and prints one line a job, in
file order, then a summary. Exits 0 when every job is done, 1 when any
failed, 2 for a file it cannot read or parse, and 3 when the deadline
passes before the jobs finish.
"""

import argparse
import os
import sys
import threading

from jobq import states
from jobq.batch import BatchError, describe_result, parse, run_batch
from jobq.report import describe

DEFAULT_DEADLINE = 900.0


def _deadline_passed():
    print("error: the deadline passed before the jobs finished", file=sys.stderr)
    sys.stderr.flush()
    os._exit(3)


def main(argv=None):
    parser = argparse.ArgumentParser(prog="python3 -m jobq.cli")
    parser.add_argument("--workers", type=int, default=2)
    parser.add_argument("--deadline", type=float, default=DEFAULT_DEADLINE,
                        help="give up after this many seconds (default %(default)s)")
    parser.add_argument("file")
    args = parser.parse_args(argv)
    watchdog = threading.Timer(args.deadline, _deadline_passed)
    watchdog.daemon = True
    watchdog.start()
    try:
        with open(args.file, encoding="utf-8") as handle:
            entries = parse(handle.read())
    except OSError as error:
        print(f"error: cannot read {args.file}: {error.strerror}", file=sys.stderr)
        return 2
    except BatchError as error:
        print(f"error: {args.file}: {error}", file=sys.stderr)
        return 2
    results = run_batch(entries, workers=args.workers)
    counts = {}
    for entry, job in results:
        print(f"{entry.id:<12} {job.state:<9} {describe_result(job)}")
        counts[job.state] = counts.get(job.state, 0) + 1
    print(describe(dict(sorted(counts.items()))))
    watchdog.cancel()
    return 1 if counts.get(states.FAILED) else 0


if __name__ == "__main__":
    sys.exit(main())
