"""Counts the lines of files: linecount.py [--words] FILE..."""

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
            text = handle.read()
        rows.append((count_lines(text), len(text.split()), path))
    return rows


def main(argv):
    words = "--words" in argv[1:]
    paths = [arg for arg in argv[1:] if arg != "--words"]
    if not paths:
        print("usage: linecount.py [--words] FILE...", file=sys.stderr)
        return 2
    rows = report(paths)
    for lines, count, path in rows:
        print(f"{lines} {count} {path}" if words else f"{lines} {path}")
    if len(rows) > 1:
        lines = sum(row[0] for row in rows)
        count = sum(row[1] for row in rows)
        print(f"{lines} {count} total" if words else f"{lines} total")
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
