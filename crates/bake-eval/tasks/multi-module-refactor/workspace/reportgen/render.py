"""Formats a table with a formatter chosen by name."""

from reportgen import lookup_cache
from reportgen.registry import aggregator_names, filter_names, formatter_names


def render(name, table, title=""):
    """The table as the formatter `name` writes it, ending in a newline."""
    text = lookup_cache.formatter(name)(table, title)
    return text if text.endswith("\n") else text + "\n"


def list_formats():
    return formatter_names()


def list_aggregators():
    return aggregator_names()


def list_filters():
    return filter_names()
