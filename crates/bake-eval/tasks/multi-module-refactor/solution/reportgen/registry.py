"""Where readers, aggregators, formatters, and filters are found by name.

A Registry holds its own sets; `reportgen.default_registry()` makes one with
the built-in ones.
"""

from reportgen.errors import ReportError

_KINDS = {"reader": "reader", "aggregator": "aggregator", "formatter": "format", "filter": "filter"}


class Registry:
    def __init__(self):
        self._tables = {kind: {} for kind in _KINDS}

    def _add(self, kind, name, fn):
        table = self._tables[kind]
        if name in table:
            raise ReportError(f"{_KINDS[kind]} {name!r} is already registered")
        table[name] = fn
        return fn

    def add_reader(self, suffix, fn):
        """Adds a reader, `fn(path) -> Table`, for files ending in `suffix`."""
        return self._add("reader", suffix, fn)

    def add_aggregator(self, name, fn):
        """Adds an aggregator, `fn(numbers) -> number`."""
        return self._add("aggregator", name, fn)

    def add_formatter(self, name, fn):
        """Adds a formatter, `fn(table, title) -> str`."""
        return self._add("formatter", name, fn)

    def add_filter(self, name, fn):
        """Adds a filter operator, `fn(cell, operand) -> bool`."""
        return self._add("filter", name, fn)

    def _get(self, kind, name):
        table = self._tables[kind]
        try:
            return table[name]
        except KeyError:
            known = ", ".join(sorted(table)) or "none"
            raise ReportError(f"unknown {_KINDS[kind]} {name!r} (known: {known})") from None

    def get_reader(self, suffix):
        return self._get("reader", suffix)

    def get_aggregator(self, name):
        return self._get("aggregator", name)

    def get_formatter(self, name):
        return self._get("formatter", name)

    def get_filter(self, name):
        return self._get("filter", name)

    def reader_suffixes(self):
        return sorted(self._tables["reader"])

    def aggregator_names(self):
        return sorted(self._tables["aggregator"])

    def formatter_names(self):
        return sorted(self._tables["formatter"])

    def filter_names(self):
        return sorted(self._tables["filter"])
