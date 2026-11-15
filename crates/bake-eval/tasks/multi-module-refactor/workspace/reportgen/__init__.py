"""Reports from tabular data: read a file, filter, group and aggregate, format."""

# Importing the built-in packages registers their readers, aggregators,
# formatters, and filters. The slower readers load on first use (lazy.py).
from reportgen import aggregators, filters, formatters, readers  # noqa: F401
from reportgen.aggregate import aggregate, group_rows
from reportgen.errors import ReportError
from reportgen.pipeline import build_report
from reportgen.plugins import load_plugin
from reportgen.render import list_aggregators, list_filters, list_formats, render
from reportgen.sources import read_table
from reportgen.spec import Spec, parse_spec
from reportgen.table import Table
from reportgen.values import format_cell, format_number, parse_number

__all__ = [
    "ReportError",
    "Spec",
    "Table",
    "aggregate",
    "build_report",
    "format_cell",
    "format_number",
    "group_rows",
    "list_aggregators",
    "list_filters",
    "list_formats",
    "load_plugin",
    "parse_number",
    "parse_spec",
    "read_table",
    "render",
]
