"""Readers that load on first use.

The tab-separated and fixed-width readers pull in more code than most
reports need, so their modules are imported only when a file with their
suffix is read. Importing the module registers the reader.
"""

import importlib

from reportgen import registry

LAZY_READERS = {
    ".tsv": "reportgen.readers.tsv",
    ".fw": "reportgen.readers.fixed",
}


def ensure_reader(suffix):
    """Imports the module of a lazy reader for `suffix` if it is not loaded yet."""
    if not registry.has_reader(suffix) and suffix in LAZY_READERS:
        importlib.import_module(LAZY_READERS[suffix])


def known_suffixes():
    """Every suffix a reader exists for, loaded or not."""
    return sorted(set(registry.reader_suffixes()) | set(LAZY_READERS))
