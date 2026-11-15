"""A Markdown table under a level-two heading."""

from reportgen.formatters.common import cells
from reportgen.registry import formatter


def _line(row):
    return "| " + " | ".join(cell.replace("|", "\\|") for cell in row) + " |"


@formatter("markdown")
def format_markdown(table, title):
    rows = cells(table)
    lines = [f"## {title}", ""] if title else []
    lines.append(_line(rows[0]))
    lines.append("|" + "|".join("---" for _ in rows[0]) + "|")
    lines.extend(_line(row) for row in rows[1:])
    return "\n".join(lines) + "\n"
