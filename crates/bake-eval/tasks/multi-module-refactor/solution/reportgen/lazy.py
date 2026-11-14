"""Readers that load on first use.

The tab-separated and fixed-width readers pull in more code than most
reports need, so their modules are imported only when a file with their
suffix is read.
"""

import importlib

LAZY_READERS = {
    ".tsv": ("reportgen.readers.tsv", "read_tsv"),
    ".fw": ("reportgen.readers.fixed", "read_fixed"),
}


def lazy_reader(module, attribute):
    """A reader that imports `module` the first time it reads a file."""

    def read(path):
        return getattr(importlib.import_module(module), attribute)(path)

    read.__name__ = attribute
    return read
