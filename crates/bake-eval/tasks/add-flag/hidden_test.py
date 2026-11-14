"""Run in the workspace: linecount.py with and without --words."""

import os
import subprocess
import sys
import tempfile

with tempfile.TemporaryDirectory() as directory:
    one = os.path.join(directory, "one.txt")
    two = os.path.join(directory, "two.txt")
    empty = os.path.join(directory, "empty.txt")
    with open(one, "w", encoding="utf-8") as handle:
        handle.write("alpha beta\n  gamma\tdelta  \n\nepsilon")
    with open(two, "w", encoding="utf-8") as handle:
        handle.write("one\ntwo words\n")
    with open(empty, "w", encoding="utf-8"):
        pass

    def run(*args):
        ran = subprocess.run([sys.executable, "linecount.py", *args], capture_output=True, text=True)
        return ran.returncode, ran.stdout

    cases = [
        ((one,), f"4 {one}\n"),
        ((one, two), f"4 {one}\n2 {two}\n6 total\n"),
        (("--words", one), f"4 5 {one}\n"),
        ((one, "--words"), f"4 5 {one}\n"),
        (("--words", one, two, empty), f"4 5 {one}\n2 3 {two}\n0 0 {empty}\n6 8 total\n"),
        ((one, "--words", two), f"4 5 {one}\n2 3 {two}\n6 8 total\n"),
    ]
    for args, expected in cases:
        status, output = run(*args)
        assert (status, output) == (0, expected), (args, status, output, expected)
