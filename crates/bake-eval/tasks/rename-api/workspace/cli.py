"""Applies a text filter to its arguments: cli.py --filter NAME TEXT..."""

import sys

from textkit.filters import apply


def main(argv):
    if len(argv) < 4 or argv[1] != "--filter":
        print("usage: cli.py --filter normalize_ws|title TEXT...", file=sys.stderr)
        return 2
    try:
        print(apply(argv[2], " ".join(argv[3:])))
    except ValueError as error:
        print(error, file=sys.stderr)
        return 2
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
