"""An example plugin: the text format in capitals, and the value range."""

from reportgen.formatters.text import format_text
from reportgen.registry import aggregator, formatter


@formatter("shout")
def shout(table, title):
    return format_text(table, title).upper()


@aggregator("range")
def spread(numbers):
    return max(numbers) - min(numbers)
