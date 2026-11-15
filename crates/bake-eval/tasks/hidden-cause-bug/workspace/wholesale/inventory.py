"""Stock and reservations: an order reserves its units until it ships."""

from wholesale import catalog


class OutOfStock(Exception):
    pass


class Inventory:
    def __init__(self, on_hand=None):
        self.on_hand = dict(on_hand or {sku: 0 for sku in catalog.PRODUCTS})
        self.reserved = {}

    def available(self, sku):
        return self.on_hand.get(sku, 0) - sum(units for (_, item), units in self.reserved.items() if item == sku)

    def reserve(self, order):
        """Reserves every line of `order`, or nothing if any line cannot be had."""
        wanted = {}
        for line in order.lines:
            wanted[line.sku] = wanted.get(line.sku, 0) + line.quantity
        short = [sku for sku, units in sorted(wanted.items()) if units > self.available(sku)]
        if short:
            raise OutOfStock(f"order {order.number}: not enough {', '.join(short)}")
        for sku, units in wanted.items():
            self.reserved[(order.number, sku)] = units

    def ship(self, order_number):
        """Takes the order's reserved units off the shelf."""
        for (number, sku), units in list(self.reserved.items()):
            if number == order_number:
                self.on_hand[sku] -= units
                del self.reserved[(number, sku)]

    def release(self, order_number):
        for key in [key for key in self.reserved if key[0] == order_number]:
            del self.reserved[key]
