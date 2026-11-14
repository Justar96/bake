"""Run in the workspace: the release was prepared exactly as the release-prep skill says."""

import os
import sys

TASK = os.environ["BAKE_EVAL_TASK_DIR"]
TEMPLATE = os.path.join(TASK, "workspace")


def read(path, root="."):
    with open(os.path.join(root, path), encoding="utf-8", newline="") as handle:
        return handle.read()


def fail(message):
    print(message)
    sys.exit(1)


if read("VERSION") != "2.3.0\n":
    fail(f"VERSION holds {read('VERSION')!r}, not the line 2.3.0")
init = read("tidy/__init__.py")
if init != read("tidy/__init__.py", TEMPLATE).replace('"2.2.1"', '"2.3.0"'):
    fail("tidy/__init__.py does not set __version__ to 2.3.0 (or changes more)")

notes = "release-notes/v2.3.0-juniper.md"
if not os.path.isdir("release-notes"):
    fail("there is no release-notes directory")
written = sorted(os.listdir("release-notes"))
if written != [os.path.basename(notes)]:
    fail(f"release-notes holds {written}, not just {os.path.basename(notes)}")
expected = read(notes, os.path.join(TASK, "solution"))
got = read(notes)
if got != expected:
    for number, (want, have) in enumerate(zip(expected.split("\n"), got.split("\n")), 1):
        if want != have:
            fail(f"{notes} line {number} is {have!r}, not {want!r}")
    fail(f"{notes} has {got.count(chr(10))} lines where {expected.count(chr(10))} are expected")

pending = read("UNRELEASED.txt")
header = "".join(line for line in read("UNRELEASED.txt", TEMPLATE).splitlines(True) if line.startswith("#"))
if pending.rstrip("\n") != header.rstrip("\n"):
    fail("UNRELEASED.txt does not hold just its comment lines")

releases = read("RELEASES.csv")
if releases != read("RELEASES.csv", TEMPLATE) + "2.3.0,juniper,8,107\n":
    fail(f"RELEASES.csv ends with {releases.splitlines()[-1]!r}, not one new row 2.3.0,juniper,8,107")
