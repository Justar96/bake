"""Plain text: a title and aligned columns."""

from reportgen.formatters.common import cells, widths


def format_text(table, title):
    rows = cells(table)
    sizes = widths(rows)
    lines = [title, "=" * len(title), ""] if title else []
    for number, row in enumerate(rows):
        lines.append("  ".join(cell.ljust(size) for cell, size in zip(row, sizes)).rstrip())
        if number == 0:
            lines.append("  ".join("-" * size for size in sizes))
    return "\n".join(lines) + "\n"
