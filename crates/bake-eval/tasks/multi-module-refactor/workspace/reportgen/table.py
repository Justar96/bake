"""A table: named columns and rows of values."""

from reportgen.errors import ReportError


class Table:
    def __init__(self, columns, rows=()):
        self.columns = tuple(columns)
        if len(set(self.columns)) != len(self.columns):
            raise ReportError(f"duplicate column in {self.columns!r}")
        self.rows = []
        for row in rows:
            self.append(row)

    def append(self, row):
        row = tuple(row)
        if len(row) != len(self.columns):
            raise ReportError(f"row {row!r} has {len(row)} values for {len(self.columns)} columns")
        self.rows.append(row)

    def index(self, column):
        try:
            return self.columns.index(column)
        except ValueError:
            raise ReportError(f"no column {column!r} (columns: {', '.join(self.columns)})") from None

    def column(self, name):
        position = self.index(name)
        return [row[position] for row in self.rows]

    def sorted_by(self, column):
        position = self.index(column)
        return Table(self.columns, sorted(self.rows, key=lambda row: row[position]))

    def __len__(self):
        return len(self.rows)

    def __eq__(self, other):
        return isinstance(other, Table) and (self.columns, self.rows) == (other.columns, other.rows)

    def __repr__(self):
        return f"Table({self.columns!r}, {self.rows!r})"
