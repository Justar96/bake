import unittest
from decimal import Decimal

from wholesale.keys import quote_key
from wholesale.quotes import QuoteCache


class QuoteCacheTest(unittest.TestCase):
    def test_groups_are_cached_apart(self):
        cache = QuoteCache()
        self.assertEqual(cache.unit_price("hinge-80", 2), Decimal("4.20"))
        self.assertEqual(cache.unit_price("hinge-80", 3, "trade"), Decimal("3.990"))
        self.assertEqual((cache.misses, cache.hits), (2, 0))

    def test_quantities_on_one_step_share_a_price(self):
        cache = QuoteCache()
        prices = {cache.unit_price("glue-750", quantity) for quantity in (30, 60, 200)}
        self.assertEqual(prices, {Decimal("6.956")})
        self.assertEqual((cache.misses, cache.hits), (1, 2))

    def test_keys(self):
        self.assertEqual(quote_key("saw-500", 2, "retail"), ("saw-500", "retail", 0))
        self.assertEqual(quote_key("saw-500", 20, "trade"), ("saw-500", "trade", 2))

    def test_clear(self):
        cache = QuoteCache()
        cache.unit_price("nut-m6", 4)
        cache.clear()
        cache.unit_price("nut-m6", 4)
        self.assertEqual(cache.misses, 2)


if __name__ == "__main__":
    unittest.main()
