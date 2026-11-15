"""Reads fixed-width files (loaded lazily).

The first line names the columns; each name starts its column, and a column
runs up to the start of the next one.
"""

from reportgen.errors import ReportError
from reportgen.table import Table


def _starts(header):
    starts = []
    for position, char in enumerate(header):
        if char != " " and (position == 0 or header[position - 1] == " "):
            starts.append(position)
    return starts


def read_fixed(path):
    with open(path, encoding="utf-8") as handle:
        lines = [line.rstrip("\n") for line in handle if line.strip()]
    if not lines:
        raise ReportError(f"{path}: no header line")
    starts = _starts(lines[0])
    bounds = list(zip(starts, starts[1:] + [None]))
    names = [lines[0][start:end].strip() for start, end in bounds]
    return Table(names, [[line[start:end].strip() for start, end in bounds] for line in lines[1:]])
