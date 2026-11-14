"""Reads files of one JSON object a line, all with the same keys."""

import json

from reportgen.errors import ReportError
from reportgen.registry import reader
from reportgen.table import Table


@reader(".jsonl")
def read_jsonl(path):
    records = []
    with open(path, encoding="utf-8") as handle:
        for number, line in enumerate(handle, 1):
            if not line.strip():
                continue
            try:
                record = json.loads(line)
            except json.JSONDecodeError as error:
                raise ReportError(f"{path}:{number}: {error.msg}") from None
            if not isinstance(record, dict):
                raise ReportError(f"{path}:{number}: not an object")
            records.append((number, record))
    if not records:
        raise ReportError(f"{path}: no records")
    columns = list(records[0][1])
    table = Table(columns)
    for number, record in records:
        if sorted(record) != sorted(columns):
            raise ReportError(f"{path}:{number}: keys differ from the first record's")
        table.append([record[name] for name in columns])
    return table
