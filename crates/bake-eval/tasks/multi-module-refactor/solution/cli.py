"""Builds a report from a spec and a data file.

    python3 cli.py [--plugin FILE]... [--format NAME] SPEC DATA
    python3 cli.py [--plugin FILE]... --list
"""

import argparse
import sys

import reportgen


def main(argv=None):
    parser = argparse.ArgumentParser(prog="cli.py", description="Build a report.")
    parser.add_argument("--plugin", action="append", default=[], metavar="FILE",
                        help="load a plugin file first; may be repeated")
    parser.add_argument("--format", help="use this format instead of the spec's")
    parser.add_argument("--list", action="store_true", help="list the formats, aggregators, and filters")
    parser.add_argument("spec", nargs="?")
    parser.add_argument("data", nargs="?")
    args = parser.parse_args(argv)
    registry = reportgen.default_registry()
    try:
        for path in args.plugin:
            reportgen.load_plugin(registry, path)
        if args.list:
            print("formats:", ", ".join(reportgen.list_formats(registry)))
            print("aggregators:", ", ".join(reportgen.list_aggregators(registry)))
            print("filters:", ", ".join(reportgen.list_filters(registry)))
            return 0
        if not args.spec or not args.data:
            parser.error("give a SPEC and a DATA file, or --list")
        try:
            with open(args.spec, encoding="utf-8") as handle:
                spec_text = handle.read()
        except OSError as error:
            raise reportgen.ReportError(f"cannot read {args.spec}: {error.strerror}") from None
        sys.stdout.write(reportgen.build_report(registry, spec_text, args.data, format=args.format))
    except reportgen.ReportError as error:
        print(f"error: {error}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
