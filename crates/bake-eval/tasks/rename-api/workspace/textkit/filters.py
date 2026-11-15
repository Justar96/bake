"""The filters the command line can apply, by name."""

from textkit import ws
from textkit.titles import title_case

FILTERS = {
    "normalize_ws": ws.normalize_ws,
    "title": title_case,
}


def apply(name, text):
    try:
        return FILTERS[name](text)
    except KeyError:
        raise ValueError(f"unknown filter {name!r}; choose from {', '.join(sorted(FILTERS))}") from None
