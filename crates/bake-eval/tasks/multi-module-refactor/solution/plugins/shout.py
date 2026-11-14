"""An example plugin: the text format in capitals, and the value range."""

from reportgen.formatters.text import format_text


def shout(table, title):
    return format_text(table, title).upper()


def spread(numbers):
    return max(numbers) - min(numbers)


def register(registry):
    registry.add_formatter("shout", shout)
    registry.add_aggregator("range", spread)
