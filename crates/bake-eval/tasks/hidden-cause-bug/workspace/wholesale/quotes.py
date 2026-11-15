"""A cache of unit prices.

The price of a product is the same for every quantity on a ladder step, so
the cache computes it once per product, customer group, and step, and
answers every other quantity on that step from memory.
"""

from wholesale import pricing
from wholesale.keys import quote_key


class QuoteCache:
    def __init__(self):
        # Entries never expire: prices only change with a new release.
        self._prices = {}
        self.hits = 0
        self.misses = 0

    def unit_price(self, sku, quantity, group="retail"):
        key = quote_key(sku, quantity, group)
        if key in self._prices:
            self.hits += 1
            return self._prices[key]
        self.misses += 1
        price = pricing.unit_price(sku, quantity, group)
        self._prices[key] = price
        return price

    def clear(self):
        self._prices.clear()
