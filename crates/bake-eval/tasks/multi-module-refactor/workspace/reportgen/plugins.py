"""Plugins: Python files that register more readers, aggregators, formatters, or filters."""

import runpy

from reportgen import lookup_cache
from reportgen.errors import ReportError


def load_plugin(path):
    """Runs the plugin file `path`, whose decorators register what it adds."""
    try:
        runpy.run_path(path)
    except OSError as error:
        raise ReportError(f"cannot load the plugin {path}: {error.strerror}") from None
    lookup_cache.clear()
