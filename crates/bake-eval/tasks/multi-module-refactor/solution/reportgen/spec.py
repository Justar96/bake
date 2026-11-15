"""Report specifications: `field: value` lines, `#` comments."""

from dataclasses import dataclass

from reportgen.errors import ReportError
from reportgen.where import parse_where

FIELDS = ("title", "group-by", "column", "aggregates", "format", "sort", "where")
REQUIRED = ("group-by", "column", "aggregates")


@dataclass(frozen=True)
class Spec:
    group_by: str
    column: str
    aggregates: tuple
    title: str = ""
    format: str = "text"
    sort: str = None
    where: object = None


def parse_spec(registry, text):
    """Parses and checks a spec; unknown aggregators, formats, or filter
    operators fail here, before any data is read."""
    values = {}
    for number, line in enumerate(text.splitlines(), 1):
        line = line.strip()
        if not line or line.startswith("#"):
            continue
        field, colon, value = line.partition(":")
        field = field.strip().lower()
        if not colon or field not in FIELDS:
            raise ReportError(f"line {number}: unknown field {field!r}")
        if field in values:
            raise ReportError(f"line {number}: {field} is given twice")
        values[field] = value.strip()
    missing = [field for field in REQUIRED if not values.get(field)]
    if missing:
        raise ReportError("the spec has no " + ", ".join(missing))
    aggregates = tuple(name.strip() for name in values["aggregates"].split(",") if name.strip())
    for name in aggregates:
        registry.get_aggregator(name)
    if len(set(aggregates)) != len(aggregates):
        raise ReportError("an aggregate is listed twice")
    form = values.get("format") or "text"
    registry.get_formatter(form)
    sort = values.get("sort") or None
    if sort is not None and sort not in (values["group-by"], *aggregates):
        raise ReportError(f"cannot sort by {sort!r}: it is not the group or an aggregate")
    where = parse_where(registry, values["where"]) if values.get("where") else None
    return Spec(
        group_by=values["group-by"],
        column=values["column"],
        aggregates=aggregates,
        title=values.get("title", ""),
        format=form,
        sort=sort,
        where=where,
    )
