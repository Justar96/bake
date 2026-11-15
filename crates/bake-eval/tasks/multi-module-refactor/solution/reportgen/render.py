"""Formats a table with a formatter chosen by name."""

from reportgen import lookup_cache


def render(registry, name, table, title=""):
    """The table as the formatter `name` writes it, ending in a newline."""
    text = lookup_cache.formatter(registry, name)(table, title)
    return text if text.endswith("\n") else text + "\n"


def list_formats(registry):
    return registry.formatter_names()


def list_aggregators(registry):
    return registry.aggregator_names()


def list_filters(registry):
    return registry.filter_names()
