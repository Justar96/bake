"""Opens a data file with the reader registered for its suffix."""

import os

from reportgen.errors import ReportError
from reportgen.lazy import ensure_reader, known_suffixes
from reportgen.registry import get_reader


def read_table(path):
    """Reads `path` into a Table, choosing the reader by the file's suffix."""
    suffix = os.path.splitext(path)[1].lower()
    ensure_reader(suffix)
    try:
        read = get_reader(suffix)
    except ReportError:
        raise ReportError(f"unknown reader {suffix!r} (known: {', '.join(known_suffixes())})") from None
    return read(path)
