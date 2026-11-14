import re

_RUN = re.compile(r"\s+")


def collapse_whitespace(text):
    """Collapses each run of whitespace to one space and trims both ends."""
    return _RUN.sub(" ", text).strip()
