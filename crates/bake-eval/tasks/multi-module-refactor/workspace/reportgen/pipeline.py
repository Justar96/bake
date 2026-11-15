"""A whole report: spec, data, grouping, sorting, formatting."""

import dataclasses

from reportgen.aggregate import group_rows
from reportgen.render import render
from reportgen.sources import read_table
from reportgen.spec import parse_spec
from reportgen.where import apply_where


def build_report(spec_text, data_path, format=None):
    """The report `spec_text` describes over the file `data_path`; `format`
    replaces the spec's format when given."""
    spec = parse_spec(spec_text)
    if format is not None:
        spec = dataclasses.replace(spec, format=format)
    table = read_table(data_path)
    if spec.where is not None:
        table = apply_where(table, spec.where)
    result = group_rows(table, spec.group_by, spec.column, spec.aggregates)
    if spec.sort:
        result = result.sorted_by(spec.sort)
    return render(spec.format, result, spec.title)
