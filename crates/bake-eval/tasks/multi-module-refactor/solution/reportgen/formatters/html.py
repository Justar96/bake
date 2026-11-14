"""An HTML table with a caption."""

from html import escape

from reportgen.formatters.common import cells


def format_html(table, title):
    rows = cells(table)
    lines = ["<table>"]
    if title:
        lines.append(f"  <caption>{escape(title)}</caption>")
    lines.append("  <tr>" + "".join(f"<th>{escape(cell)}</th>" for cell in rows[0]) + "</tr>")
    for row in rows[1:]:
        lines.append("  <tr>" + "".join(f"<td>{escape(cell)}</td>" for cell in row) + "</tr>")
    lines.append("</table>")
    return "\n".join(lines) + "\n"
