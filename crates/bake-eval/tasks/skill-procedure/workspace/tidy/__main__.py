"""python3 -m tidy [--width N] FILE...: rewrites each file tidied."""

import argparse
import sys

from tidy.format import tidy_text


def main(argv=None):
    parser = argparse.ArgumentParser(prog="tidy")
    parser.add_argument("--width", type=int, default=79)
    parser.add_argument("files", nargs="+")
    args = parser.parse_args(argv)
    for path in args.files:
        with open(path, encoding="utf-8") as handle:
            text = handle.read()
        with open(path, "w", encoding="utf-8") as handle:
            handle.write(tidy_text(text, args.width))
    return 0


if __name__ == "__main__":
    sys.exit(main())
