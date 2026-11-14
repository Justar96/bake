"""Opens a data file with the reader registered for its suffix."""

import os


def read_table(registry, path):
    """Reads `path` into a Table, choosing the reader by the file's suffix."""
    suffix = os.path.splitext(path)[1].lower()
    return registry.get_reader(suffix)(path)
