"""Counts the lines of files: linecount.py FILE..."""

import sys


def count_lines(text):
    """The number of lines in text; a last line without a newline counts."""
    if not text:
        return 0
    return text.count("\n") + (0 if text.endswith("\n") else 1)


def report(paths):
    rows = []
    for path in paths:
        with open(path, encoding="utf-8") as handle:
            rows.append((count_lines(handle.read()), path))
    return rows


def main(argv):
    paths = argv[1:]
    if not paths:
        print("usage: linecount.py FILE...", file=sys.stderr)
        return 2
    rows = report(paths)
    for lines, path in rows:
        print(f"{lines} {path}")
    if len(rows) > 1:
        print(f"{sum(lines for lines, _ in rows)} total")
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
