import unittest
from decimal import Decimal

from wholesale import pricing
from wholesale.ladders import ladder, ladder_of


class PricingTest(unittest.TestCase):
    def test_volume_discounts(self):
        self.assertEqual([pricing.volume_discount("bolt-m6", q) for q in (1, 9, 20, 75, 400)], [0, 0, 5, 10, 15])
        self.assertEqual([pricing.volume_discount("hinge-80", q) for q in (1, 4, 6, 30)], [0, 0, 4, 8])

    def test_unit_prices(self):
        self.assertEqual(pricing.unit_price("bolt-m6", 20), Decimal("0.38"))
        self.assertEqual(pricing.unit_price("bolt-m6", 20, "trade"), Decimal("0.3610"))
        self.assertEqual(pricing.unit_price("saw-500", 1, "trade"), Decimal("17.955"))

    def test_errors(self):
        with self.assertRaises(pricing.PricingError):
            pricing.unit_price("bolt-m6", 0)
        with self.assertRaises(pricing.PricingError):
            pricing.unit_price("rivet", 3)
        with self.assertRaises(pricing.PricingError):
            pricing.unit_price("bolt-m6", 3, "vip")

    def test_ladders(self):
        self.assertEqual(ladder_of("glue-750").family, "consumables")
        self.assertEqual([ladder("tools").label(step) for step in range(3)], ["1-2", "3-11", "12+"])
        self.assertEqual(ladder("hardware").step_of(30), 2)


if __name__ == "__main__":
    unittest.main()
