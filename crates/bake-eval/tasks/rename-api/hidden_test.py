"""Run in the workspace: the renamed API works and the old one is gone."""

import os
import subprocess
import sys

sys.path.insert(0, os.getcwd())

import textkit
from textkit import collapse_whitespace, find_phrase, title_case
from textkit.filters import FILTERS, apply

assert collapse_whitespace("  a \t b\n\nc ") == "a b c"
assert title_case("the  lord of the rings") == "The Lord of the Rings"
assert find_phrase("One  Two   Three", "two three") == 4
assert not hasattr(textkit, "normalize_ws"), "textkit still exports normalize_ws"
assert "collapse_whitespace" in textkit.__all__
assert sorted(FILTERS) == ["collapse_whitespace", "title"], sorted(FILTERS)
assert apply("collapse_whitespace", " x  y ") == "x y"
ran = subprocess.run(
    [sys.executable, "cli.py", "--filter", "collapse_whitespace", "  too   many ", "spaces "],
    capture_output=True, text=True,
)
assert ran.returncode == 0 and ran.stdout == "too many spaces\n", (ran.returncode, ran.stdout, ran.stderr)
