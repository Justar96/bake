"""Reads tab-separated files with a header row (loaded lazily)."""

import csv

from reportgen.readers.csv_reader import rows_to_table


def read_tsv(path):
    with open(path, newline="", encoding="utf-8") as handle:
        return rows_to_table(path, list(csv.reader(handle, delimiter="\t")))
