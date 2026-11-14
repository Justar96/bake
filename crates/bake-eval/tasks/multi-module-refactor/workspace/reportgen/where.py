"""A spec's `where` line: `<column> <operator> <operand>`, such as
`amount > 100` or `product contains apple`."""

from dataclasses import dataclass

from reportgen import lookup_cache
from reportgen.errors import ReportError
from reportgen.table import Table


@dataclass(frozen=True)
class Condition:
    column: str
    operator: str
    operand: str


def parse_where(text):
    parts = text.split(None, 2)
    if len(parts) != 3:
        raise ReportError(f"where needs <column> <operator> <operand>, not {text!r}")
    column, operator, operand = parts
    lookup_cache.filter_operator(operator)
    return Condition(column, operator, operand)


def apply_where(table, condition):
    """The rows of `table` that meet `condition`."""
    test = lookup_cache.filter_operator(condition.operator)
    position = table.index(condition.column)
    return Table(table.columns, [row for row in table.rows if test(row[position], condition.operand)])
