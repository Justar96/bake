"""Reads comma-separated files with a header row."""

import csv

from reportgen.errors import ReportError
from reportgen.registry import reader
from reportgen.table import Table


def rows_to_table(path, rows):
    rows = [row for row in rows if row]
    if not rows:
        raise ReportError(f"{path}: no header row")
    header = [name.strip() for name in rows[0]]
    return Table(header, [[cell.strip() for cell in row] for row in rows[1:]])


@reader(".csv")
def read_csv(path):
    with open(path, newline="", encoding="utf-8") as handle:
        return rows_to_table(path, list(csv.reader(handle)))
