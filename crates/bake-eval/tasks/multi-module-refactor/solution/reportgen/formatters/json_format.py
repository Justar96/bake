"""A JSON object with the title, the columns, and the rows as text."""

import json

from reportgen.formatters.common import cells


def format_json(table, title):
    rows = cells(table)
    return json.dumps({"title": title, "columns": rows[0], "rows": rows[1:]}, indent=2) + "\n"
