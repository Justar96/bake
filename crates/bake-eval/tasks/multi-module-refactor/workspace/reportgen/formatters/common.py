"""Helpers the formatters share."""

from reportgen.values import format_cell


def cells(table):
    """The header and the rows of `table` as text."""
    return [list(table.columns)] + [[format_cell(value) for value in row] for row in table.rows]


def widths(rows):
    return [max(len(row[i]) for row in rows) for i in range(len(rows[0]))]
