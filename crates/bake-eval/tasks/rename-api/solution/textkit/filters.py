"""The filters the command line can apply, by name."""

from textkit import ws
from textkit.titles import title_case

FILTERS = {
    "collapse_whitespace": ws.collapse_whitespace,
    "title": title_case,
}


def apply(name, text):
    try:
        return FILTERS[name](text)
    except KeyError:
        raise ValueError(f"unknown filter {name!r}; choose from {', '.join(sorted(FILTERS))}") from None
