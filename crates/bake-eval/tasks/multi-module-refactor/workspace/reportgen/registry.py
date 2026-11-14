"""Where readers, aggregators, formatters, and filters are found by name.

The built-in ones register themselves with the decorators below when their
modules are imported; plugins use the same decorators.
"""

from reportgen.errors import ReportError

_READERS = {}
_AGGREGATORS = {}
_FORMATTERS = {}
_FILTERS = {}


def _register(table, kind, name):
    def decorate(fn):
        if name in table:
            raise ReportError(f"{kind} {name!r} is already registered")
        table[name] = fn
        return fn

    return decorate


def reader(suffix):
    """Registers a reader, `fn(path) -> Table`, for files ending in `suffix`."""
    return _register(_READERS, "reader", suffix)


def aggregator(name):
    """Registers an aggregator, `fn(numbers) -> number`."""
    return _register(_AGGREGATORS, "aggregator", name)


def formatter(name):
    """Registers a formatter, `fn(table, title) -> str`."""
    return _register(_FORMATTERS, "format", name)


def filter_operator(name):
    """Registers a filter operator, `fn(cell, operand) -> bool`."""
    return _register(_FILTERS, "filter", name)


def _lookup(table, kind, name):
    try:
        return table[name]
    except KeyError:
        known = ", ".join(sorted(table)) or "none"
        raise ReportError(f"unknown {kind} {name!r} (known: {known})") from None


def has_reader(suffix):
    return suffix in _READERS


def get_reader(suffix):
    return _lookup(_READERS, "reader", suffix)


def get_aggregator(name):
    return _lookup(_AGGREGATORS, "aggregator", name)


def get_formatter(name):
    return _lookup(_FORMATTERS, "format", name)


def get_filter(name):
    return _lookup(_FILTERS, "filter", name)


def reader_suffixes():
    return sorted(_READERS)


def aggregator_names():
    return sorted(_AGGREGATORS)


def formatter_names():
    return sorted(_FORMATTERS)


def filter_names():
    return sorted(_FILTERS)
