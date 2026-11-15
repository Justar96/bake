import os
import subprocess
import sys
import unittest
from decimal import Decimal

import reportgen
from reportgen.registry import aggregator

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.dirname(HERE)
CSV = os.path.join(HERE, "data", "sales.csv")
JSONL = os.path.join(HERE, "data", "sales.jsonl")
TSV = os.path.join(HERE, "data", "sales.tsv")
FIXED = os.path.join(HERE, "data", "sales.fw")

SPEC = """
title: Sales by region
group-by: region
column: amount
aggregates: sum, count
"""


class ValuesTest(unittest.TestCase):
    def test_numbers_parse_with_thousands_separators(self):
        self.assertEqual(reportgen.parse_number("1,200.50"), Decimal("1200.50"))
        self.assertEqual(reportgen.parse_number(12), Decimal(12))
        self.assertEqual(reportgen.parse_number(0.05), Decimal("0.05"))
        with self.assertRaises(reportgen.ReportError):
            reportgen.parse_number("twelve")

    def test_cells_format_counts_as_integers(self):
        self.assertEqual(reportgen.format_cell(3), "3")
        self.assertEqual(reportgen.format_cell(Decimal("2.005")), "2.00")
        self.assertEqual(reportgen.format_cell("north"), "north")


class ReadTest(unittest.TestCase):
    def test_csv_and_jsonl_read_the_same_columns(self):
        csv_table = reportgen.read_table(CSV)
        jsonl_table = reportgen.read_table(JSONL)
        self.assertEqual(csv_table.columns, ("region", "product", "amount"))
        self.assertEqual(jsonl_table.columns, csv_table.columns)
        self.assertEqual(len(csv_table), 7)
        self.assertEqual(len(jsonl_table), 7)

    def test_lazy_readers_read_the_same_rows(self):
        rows = reportgen.read_table(CSV).rows
        self.assertEqual(reportgen.read_table(TSV).rows, rows)
        self.assertEqual(reportgen.read_table(FIXED).rows, rows)

    def test_an_unknown_suffix_is_refused(self):
        with self.assertRaises(reportgen.ReportError):
            reportgen.read_table(os.path.join(HERE, "data", "sales.xml"))


class AggregateTest(unittest.TestCase):
    def test_builtin_aggregators(self):
        values = ["1", "2", "3", "10"]
        self.assertEqual(reportgen.aggregate("sum", values), Decimal(16))
        self.assertEqual(reportgen.aggregate("count", values), 4)
        self.assertEqual(reportgen.aggregate("mean", values), Decimal(4))
        self.assertEqual(reportgen.aggregate("median", values), Decimal("2.5"))
        self.assertEqual(reportgen.aggregate("max", values), Decimal(10))

    def test_spread_and_rank_aggregators(self):
        values = ["4", "1", "4", "9", "2"]
        self.assertEqual(reportgen.aggregate("distinct", values), 4)
        self.assertEqual(reportgen.aggregate("mode", values), Decimal(4))
        self.assertEqual(reportgen.aggregate("p75", values), Decimal(4))
        self.assertEqual(reportgen.aggregate("stdev", ["2", "4", "4", "4", "5", "5", "7", "9"]), Decimal(2))

    def test_groups_keep_their_first_order(self):
        table = reportgen.read_table(CSV)
        grouped = reportgen.group_rows(table, "region", "amount", ("sum",))
        self.assertEqual(grouped.column("region"), ["north", "south", "east"])
        self.assertEqual(grouped.column("sum"), [Decimal("1300.50"), Decimal("550.25"), Decimal("422.10")])

    def test_a_registered_aggregator_is_used(self):
        if "spread" not in reportgen.list_aggregators():
            aggregator("spread")(lambda numbers: max(numbers) - min(numbers))
        self.assertEqual(reportgen.aggregate("spread", ["4", "9", "1"]), Decimal(8))


class SpecTest(unittest.TestCase):
    def test_a_spec_parses(self):
        spec = reportgen.parse_spec(SPEC + "sort: sum\nformat: csv\n")
        self.assertEqual(spec.aggregates, ("sum", "count"))
        self.assertEqual((spec.group_by, spec.column, spec.format, spec.sort), ("region", "amount", "csv", "sum"))

    def test_unknown_names_fail_early(self):
        with self.assertRaises(reportgen.ReportError):
            reportgen.parse_spec(SPEC.replace("count", "average"))
        with self.assertRaises(reportgen.ReportError):
            reportgen.parse_spec(SPEC + "format: pdf\n")


class WhereTest(unittest.TestCase):
    def test_a_where_line_filters_rows(self):
        report = reportgen.build_report(SPEC + "where: product contains APP\nformat: csv\n", CSV)
        self.assertEqual(report, "region,sum,count\nnorth,1200.50,1\neast,410.10,1\nsouth,250.25,1\n")

    def test_numbers_compare_as_numbers(self):
        report = reportgen.build_report(SPEC + "where: amount < 100\nformat: csv\n", JSONL)
        self.assertEqual(report, "region,sum,count\nnorth,100.00,2\neast,12.00,1\n")

    def test_an_unknown_operator_fails_early(self):
        with self.assertRaises(reportgen.ReportError):
            reportgen.parse_spec(SPEC + "where: amount ~ 3\n")

    def test_filters_are_listed(self):
        self.assertIn("contains", reportgen.list_filters())


class ReportTest(unittest.TestCase):
    def test_text_report(self):
        report = reportgen.build_report(SPEC + "sort: region\n", CSV)
        self.assertEqual(
            report,
            "Sales by region\n"
            "===============\n"
            "\n"
            "region  sum      count\n"
            "------  -------  -----\n"
            "east    422.10   2\n"
            "north   1300.50  3\n"
            "south   550.25   2\n",
        )

    def test_every_format_renders(self):
        for name in ("text", "csv", "markdown", "json", "html"):
            self.assertIn("north", reportgen.build_report(SPEC, JSONL, format=name), name)

    def test_formats_are_listed(self):
        for name in ("csv", "json", "markdown", "text"):
            self.assertIn(name, reportgen.list_formats())


class CliTest(unittest.TestCase):
    def run_cli(self, *args):
        return subprocess.run(
            [sys.executable, "cli.py", *args], cwd=ROOT, capture_output=True, text=True
        )

    def test_the_example_report(self):
        ran = self.run_cli("--format", "csv", "examples/by-region.spec", CSV)
        self.assertEqual(ran.returncode, 0, ran.stderr)
        self.assertEqual(
            ran.stdout,
            "region,sum,count,mean\neast,422.10,2,211.05\nnorth,1300.50,3,433.50\nsouth,550.25,2,275.12\n",
        )

    def test_a_plugin_adds_a_format(self):
        ran = self.run_cli("--plugin", "plugins/shout.py", "--list")
        self.assertEqual(ran.returncode, 0, ran.stderr)
        self.assertIn("shout", ran.stdout)
        self.assertIn("range", ran.stdout)

    def test_errors_are_reported(self):
        ran = self.run_cli("--format", "pdf", "examples/by-region.spec", CSV)
        self.assertEqual(ran.returncode, 1)
        self.assertEqual(ran.stderr, "error: unknown format 'pdf' (known: csv, html, json, markdown, text)\n")


if __name__ == "__main__":
    unittest.main()
