"""The one error reportgen raises for bad input."""


class ReportError(Exception):
    """A report cannot be built: bad data, a bad spec, or an unknown name."""
