"""Run in the workspace: the registry is an explicit object and the behaviour is unchanged."""

import importlib
import json
import os
import pkgutil
import subprocess
import sys
import tempfile
from decimal import Decimal

sys.path.insert(0, os.getcwd())
TASK = os.environ["BAKE_EVAL_TASK_DIR"]

import reportgen  # noqa: E402

# Importing every module registers nothing anywhere.
for module in pkgutil.walk_packages(reportgen.__path__, "reportgen."):
    importlib.import_module(module.name)

CSV = "tests/data/sales.csv"
JSONL = "tests/data/sales.jsonl"
BY_PRODUCT = "title: By product\ngroup-by: product\ncolumn: amount\naggregates: median, min, max, sum\nsort: sum\n"
EXPECTED = {
    "text": "By product\n==========\n\nproduct  median  min     max      sum\n-------  ------  ------  -------  -------\nplums    6.02    0.05    12.00    12.05\npears    199.98  99.95   300.00   399.95\napples   410.10  250.25  1200.50  1860.85\n",
    "csv": "product,median,min,max,sum\nplums,6.02,0.05,12.00,12.05\npears,199.98,99.95,300.00,399.95\napples,410.10,250.25,1200.50,1860.85\n",
    "markdown": "## By product\n\n| product | median | min | max | sum |\n|---|---|---|---|---|\n| plums | 6.02 | 0.05 | 12.00 | 12.05 |\n| pears | 199.98 | 99.95 | 300.00 | 399.95 |\n| apples | 410.10 | 250.25 | 1200.50 | 1860.85 |\n",
}


def fails(fn, *args, contains=""):
    try:
        fn(*args)
    except reportgen.ReportError as error:
        assert contains in str(error), (fn.__name__, str(error), contains)
        return
    raise AssertionError(f"{fn.__name__}{args!r} did not raise ReportError")


Registry = reportgen.Registry
default = reportgen.default_registry

one, two = default(), default()
assert isinstance(one, Registry) and one is not two
assert reportgen.list_formats(one) == ["csv", "html", "json", "markdown", "text"], reportgen.list_formats(one)
assert reportgen.list_aggregators(one) == ["count", "distinct", "max", "mean", "median", "min", "mode", "p25", "p75", "p90", "stdev", "sum"]
assert reportgen.list_filters(one) == ["!=", "<", "<=", "=", ">", ">=", "contains", "startswith"]

# A new Registry is empty: nothing comes from module state.
empty = Registry()
assert reportgen.list_formats(empty) == [] and reportgen.list_aggregators(empty) == [] and reportgen.list_filters(empty) == []
for path in (CSV, JSONL, "tests/data/sales.tsv", "tests/data/sales.fw"):
    fails(reportgen.read_table, empty, path)
fails(reportgen.read_table, empty, CSV)
fails(reportgen.aggregate, empty, "sum", ["1"])
fails(reportgen.parse_spec, empty, BY_PRODUCT)

# Every public function takes the registry first and keeps its behaviour.
for form, text in EXPECTED.items():
    assert reportgen.build_report(one, BY_PRODUCT, JSONL, format=form) == text, form
    assert reportgen.build_report(one, BY_PRODUCT + f"format: {form}\n", CSV) == text, form
table = reportgen.read_table(one, CSV)
assert table.columns == ("region", "product", "amount") and len(table) == 7
assert reportgen.aggregate(one, "median", ["1", 2, 3.5, "10"]) == Decimal("2.75")
grouped = reportgen.group_rows(one, table, "region", "amount", ("count", "max"))
assert grouped.rows == [("north", 3, Decimal("1200.50")), ("south", 2, Decimal(300)), ("east", 2, Decimal("410.10"))], grouped.rows
assert reportgen.render(one, "csv", grouped, "ignored") == "region,count,max\nnorth,3,1200.50\nsouth,2,300.00\neast,2,410.10\n"
spec = reportgen.parse_spec(one, BY_PRODUCT)
assert spec.aggregates == ("median", "min", "max", "sum") and spec.sort == "sum"
assert reportgen.parse_number("1,000") == Decimal(1000)
assert reportgen.format_cell(Decimal("1.005")) == "1.00" and reportgen.format_number(4) == "4"

# Additions stay in their registry.
one.add_aggregator("range", lambda numbers: max(numbers) - min(numbers))
one.add_formatter("plain", lambda table, title: "\n".join(",".join(map(str, row)) for row in table.rows))
assert "range" in reportgen.list_aggregators(one) and "plain" in reportgen.list_formats(one)
for other in (two, default()):
    assert "range" not in reportgen.list_aggregators(other)
    assert "plain" not in reportgen.list_formats(other)
spread = "group-by: region\ncolumn: amount\naggregates: range\nformat: plain\n"
assert reportgen.build_report(one, spread, CSV) == "north,1200.45\nsouth,49.75\neast,398.10\n"
fails(reportgen.parse_spec, two, spread, contains="(known: count, distinct, max, mean, median, min, mode, p25, p75, p90, stdev, sum)")

# A registry built from nothing works with only what it was given.
bare = Registry()
bare.add_reader(".csv", lambda path: reportgen.Table(["k", "v"], [["a", "1"], ["b", "2"], ["a", "3"]]))
bare.add_aggregator("total", lambda numbers: sum(numbers))
bare.add_formatter("plain", lambda table, title: "|".join(f"{k}={v}" for k, v in table.rows))
assert reportgen.build_report(bare, "group-by: k\ncolumn: v\naggregates: total\nformat: plain\n", "any.csv") == "a=4|b=2\n"
fails(reportgen.render, bare, "text", reportgen.Table(["k"]), "", contains="(known: plain)")
fails(reportgen.read_table, bare, JSONL, contains="(known: .csv)")
fails(reportgen.build_report, bare, "group-by: k\ncolumn: v\naggregates: sum\nformat: plain\n", "any.csv")

# Lazily loaded readers belong to the registry that reads with them.
lazy = default()
for path in ("tests/data/sales.tsv", "tests/data/sales.fw"):
    assert reportgen.read_table(lazy, path).rows == reportgen.read_table(lazy, CSV).rows, path
custom = Registry()
custom.add_reader(".tsv", lambda path: reportgen.Table(["k", "v"], [["x", "5"]]))
assert reportgen.read_table(custom, "tests/data/sales.tsv").rows == [("x", "5")]
fails(reportgen.read_table, custom, "tests/data/sales.fw")

# Lookups are never shared between registries, in either order, and a name
# added after a failed lookup is found.
first, second = Registry(), Registry()
first.add_formatter("plain", lambda table, title: "first")
second.add_formatter("plain", lambda table, title: "second")
first.add_aggregator("pick", lambda numbers: Decimal(1))
second.add_aggregator("pick", lambda numbers: Decimal(2))
first.add_filter("~", lambda cell, operand: True)
second.add_filter("~", lambda cell, operand: False)
tiny = reportgen.Table(["k"], [["a"]])
for _ in range(2):
    for registry, word, number in ((first, "first", 1), (second, "second", 2), (first, "first", 1)):
        assert reportgen.render(registry, "plain", tiny) == word + "\n", word
        assert reportgen.aggregate(registry, "pick", ["9"]) == Decimal(number), word
late = Registry()
fails(reportgen.render, late, "plain", tiny)
late.add_formatter("plain", lambda table, title: "late")
assert reportgen.render(late, "plain", tiny) == "late\n"
for registry, kept in ((first, 7), (second, 0)):
    registry.add_reader(".csv", lambda path: reportgen.read_table(one, path))
    registry.add_aggregator("count", lambda numbers: len(numbers))
    report = reportgen.build_report(registry, "group-by: region\ncolumn: amount\naggregates: count\nwhere: amount ~ x\nformat: plain\n", CSV)
    assert report == ("first" if kept else "second") + "\n", report
filtered = default()
assert reportgen.build_report(filtered, "group-by: region\ncolumn: amount\naggregates: count\nwhere: product startswith pl\nformat: csv\n", CSV) == "region,count\nnorth,1\neast,1\n"
filtered.add_filter("never", lambda cell, operand: False)
assert reportgen.build_report(filtered, "group-by: region\ncolumn: amount\naggregates: count\nwhere: amount never 1\nformat: csv\n", CSV) == "region,count\n"
fails(reportgen.parse_spec, default(), "group-by: region\ncolumn: amount\naggregates: count\nwhere: amount never 1\n", contains="never")

# Plugins add to the registry they are given, and only when loaded.
plugged = default()
reportgen.load_plugin(plugged, "plugins/shout.py")
assert "shout" in reportgen.list_formats(plugged) and "range" in reportgen.list_aggregators(plugged)
assert "shout" not in reportgen.list_formats(default())
fails(reportgen.load_plugin, default(), "plugins/missing.py")

# The command line prints what it printed before the change.
with open(os.path.join(TASK, "cli_expected.json"), encoding="utf-8") as handle:
    cases = json.load(handle)
with tempfile.TemporaryDirectory() as scratch:
    spread_spec = os.path.join(scratch, "spread.spec")
    with open(spread_spec, "w", encoding="utf-8") as handle:
        handle.write("title: Spread by region\ngroup-by: region\ncolumn: amount\naggregates: range, count\nformat: shout\nsort: range\n")
    for case in cases:
        args = [spread_spec if arg == "SPREAD" else arg for arg in case["args"]]
        ran = subprocess.run([sys.executable, "cli.py", *args], capture_output=True, text=True)
        got = (ran.returncode, ran.stdout, ran.stderr.replace(spread_spec, "SPREAD"))
        assert got == (case["code"], case["stdout"], case["stderr"]), (case["args"], got)

print("hidden tests passed")
