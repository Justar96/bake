"""Plugins: Python files that add readers, aggregators, formatters, or filters."""

import runpy

from reportgen.errors import ReportError


def load_plugin(registry, path):
    """Runs the plugin file `path` and calls its `register(registry)`."""
    try:
        namespace = runpy.run_path(path)
    except OSError as error:
        raise ReportError(f"cannot load the plugin {path}: {error.strerror}") from None
    register = namespace.get("register")
    if not callable(register):
        raise ReportError(f"the plugin {path} has no register(registry) function")
    register(registry)
