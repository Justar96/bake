# reportgen

Builds small reports from tabular files: keep the rows that match a
condition, group them by one column, aggregate another, and write the
result as text, CSV, Markdown, JSON, or HTML.

```sh
python3 cli.py examples/by-region.spec tests/data/sales.csv
python3 cli.py --format markdown examples/big-sales.spec tests/data/sales.jsonl
python3 cli.py --plugin plugins/shout.py --list
```

Readers: `.csv` and `.jsonl`, loaded with the package, and `.tsv` and
`.fw` (fixed width), loaded the first time a file needs them.

## Specs

A spec is `field: value` lines; `#` starts a comment.

| Field | Meaning |
|---|---|
| `group-by` | the column whose values make the groups (required) |
| `column` | the column to aggregate (required) |
| `aggregates` | aggregator names, separated by commas (required) |
| `title` | the report's title |
| `format` | `text` (the default), `csv`, `markdown`, `json`, or `html` |
| `sort` | the group column or an aggregate to sort the rows by |
| `where` | `<column> <operator> <operand>`: keep only the rows that match |

Aggregators: `sum`, `count`, `min`, `max`, `mean`, `median`, `stdev`,
`distinct`, `mode`, `p25`, `p75`, `p90`. Filter operators: `=`, `!=`,
`<`, `>`, `<=`, `>=`, `contains`, `startswith`.

## Extending

Readers, aggregators, formatters, and filter operators are found by name in
a `reportgen.Registry`. `reportgen.default_registry()` returns a new
registry holding the built-in ones, and every function that looks a name up
takes the registry as its first argument:

```python
import reportgen

registry = reportgen.default_registry()
registry.add_aggregator("range", lambda numbers: max(numbers) - min(numbers))
print(reportgen.build_report(registry, spec_text, "sales.csv"))
```

A plugin is a Python file with a `register(registry)` function that adds to
the registry it is given; load it with `--plugin FILE` or
`reportgen.load_plugin(registry, path)`.

## Tests

```sh
python3 -m unittest discover -s tests -t .
```
