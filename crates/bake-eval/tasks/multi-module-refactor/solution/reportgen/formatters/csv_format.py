"""Comma-separated values with a header row; the title is left out."""

import csv
import io

from reportgen.formatters.common import cells


def format_csv(table, title):
    out = io.StringIO()
    csv.writer(out, lineterminator="\n").writerows(cells(table))
    return out.getvalue()
