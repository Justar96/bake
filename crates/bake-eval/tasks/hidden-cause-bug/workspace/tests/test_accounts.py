import unittest
from decimal import Decimal

from wholesale import credit, currency
from wholesale.inventory import Inventory, OutOfStock
from wholesale.labels import label
from wholesale.orders import Line, Order
from wholesale.statements import Statement


class CreditTest(unittest.TestCase):
    def test_limits(self):
        self.assertEqual(credit.limit_of("c08"), Decimal("1500.00"))
        self.assertEqual(credit.limit_of("c04"), Decimal("4000.00"))
        self.assertEqual(credit.check("c08", Decimal("1000.00"), Decimal("500.00")), Decimal("0.00"))
        with self.assertRaises(credit.CreditHold):
            credit.check("c08", Decimal("1000.00"), Decimal("500.01"))
        self.assertEqual(credit.headroom("c08", Decimal("2000")), Decimal("0.00"))


class CurrencyTest(unittest.TestCase):
    def test_conversion_rounds_half_to_even(self):
        self.assertEqual(currency.convert(Decimal("11.683"), "GBP"), Decimal("10.00"))
        self.assertEqual(currency.show(Decimal("1041.2"), "CHF"), "1,000.00 CHF")
        with self.assertRaises(ValueError):
            currency.convert(1, "USD")


class StatementTest(unittest.TestCase):
    def test_balance_and_overdue(self):
        statement = Statement("c08")
        statement.bill(1, 1, Decimal("100.00"))
        statement.bill(2, 10, Decimal("50.00"))
        statement.payment("p1", 12, Decimal("120.00"))
        self.assertEqual(statement.balance(), Decimal("30.00"))
        self.assertEqual(statement.overdue(today=30), [("Invoice 2", Decimal("30.00"))])
        self.assertIn("Balance", statement.render())


class InventoryTest(unittest.TestCase):
    def test_reservations(self):
        stock = Inventory({"bolt-m6": 100, "nut-m6": 10})
        stock.reserve(Order(1, "c01", (Line("bolt-m6", 60), Line("nut-m6", 5))))
        with self.assertRaises(OutOfStock):
            stock.reserve(Order(2, "c01", (Line("bolt-m6", 50),)))
        stock.ship(1)
        self.assertEqual((stock.available("bolt-m6"), stock.on_hand["nut-m6"]), (40, 5))


class LabelTest(unittest.TestCase):
    def test_a_label(self):
        text = label(Order(9, "c02", (Line("clamp-f", 40),)))
        self.assertEqual(text, "BAUHOF HUBER\nGERMANY\nORDER 9  28.0 KG  2 PARCELS\n")


if __name__ == "__main__":
    unittest.main()
