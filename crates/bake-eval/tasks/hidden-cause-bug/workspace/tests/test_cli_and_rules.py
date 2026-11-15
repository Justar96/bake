import os
import subprocess
import sys
import unittest
from decimal import Decimal

from wholesale import history, rules
from wholesale.orders import Line, Order

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))


class RuleTest(unittest.TestCase):
    def test_rules(self):
        order = Order(1, "c01", (Line("washer-m6", 15), Line("bolt-m6", 4)))
        self.assertEqual(rules.problems(order, Decimal("12.00")), [
            "the subtotal 12.00 is below the trade minimum of 25.00",
            "line 1: washer-m6 ships in boxes of 10, not 15 units",
        ])
        self.assertEqual(rules.problems(Order(2, "c08", (Line("washer-m6", 20),)), Decimal("1.00")), [])
        with self.assertRaises(rules.RuleError):
            rules.enforce(order, Decimal("12.00"))


class HistoryTest(unittest.TestCase):
    def test_base_prices(self):
        self.assertEqual(history.base_price_in("bolt-m6", "2024.1"), Decimal("0.36"))
        self.assertEqual(history.base_price_in("saw-500", "2025.1"), Decimal("17.50"))
        self.assertEqual(history.base_price_in("nut-m6", "2025.2"), Decimal("0.15"))


class CliTest(unittest.TestCase):
    def run_cli(self, *args):
        return subprocess.run([sys.executable, "-m", "wholesale.cli", *args], cwd=ROOT, capture_output=True, text=True)

    def test_usage(self):
        self.assertEqual(self.run_cli().returncode, 2)

    def test_a_label(self):
        ran = self.run_cli("label", "tests/data/orders.csv", "2001")
        self.assertEqual(ran.returncode, 0, ran.stderr)
        self.assertEqual(len(ran.stdout.splitlines()), 3)

    def test_the_export_has_a_row_per_order(self):
        ran = self.run_cli("export", "tests/data/orders.csv")
        self.assertEqual(ran.returncode, 0, ran.stderr)
        self.assertEqual(len(ran.stdout.splitlines()), 71)


if __name__ == "__main__":
    unittest.main()
