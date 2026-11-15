"""Applies aggregators to the values of a column, by group."""

from reportgen import lookup_cache
from reportgen.table import Table
from reportgen.values import parse_number


def aggregate(name, values):
    """Applies the aggregator `name` to `values`, numbers or numeric strings."""
    return lookup_cache.aggregator(name)([parse_number(value) for value in values])


def group_rows(table, key, column, names):
    """One row for each distinct value of `key`, in order of first appearance,
    with a column for each aggregator in `names` applied to `column`."""
    key_at = table.index(key)
    column_at = table.index(column)
    groups = {}
    for row in table.rows:
        groups.setdefault(row[key_at], []).append(row[column_at])
    result = Table([key, *names])
    for value, members in groups.items():
        result.append([value, *(aggregate(name, members) for name in names)])
    return result
