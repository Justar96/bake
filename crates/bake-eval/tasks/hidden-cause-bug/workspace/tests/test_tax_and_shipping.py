import unittest
from decimal import Decimal

from wholesale import shipping, tax
from wholesale.regions import country_of


class TaxTest(unittest.TestCase):
    def test_country_rates(self):
        self.assertEqual(tax.rate_for("de"), Decimal(19))
        self.assertEqual(tax.rate_for(" NL "), Decimal(21))

    def test_a_levy(self):
        self.assertEqual(tax.rate_for("fr_idf"), Decimal("20.5"))

    def test_unknown_countries(self):
        with self.assertRaises(tax.TaxError):
            tax.rate_for("us")


class ShippingTest(unittest.TestCase):
    def test_countries(self):
        self.assertEqual(country_of("de-by"), "DE")
        self.assertEqual(country_of("fr idf"), "FR")

    def test_bands(self):
        self.assertEqual(shipping.cost("de", 800, Decimal(10)), Decimal("4.90"))
        self.assertEqual(shipping.cost("be-bru", 4200, Decimal(10)), Decimal("12.90"))
        self.assertEqual(shipping.cost("at", 23500, Decimal(10)), Decimal("15.70"))

    def test_free_shipping(self):
        self.assertEqual(shipping.cost("fr", 30000, Decimal("500.00")), Decimal("0.00"))


if __name__ == "__main__":
    unittest.main()
