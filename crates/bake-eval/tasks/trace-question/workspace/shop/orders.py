"""Orders placed from carts."""

from dataclasses import dataclass

from shop.pricing import price


@dataclass
class Order:
    parts: dict

    def total(self):
        return sum(self.parts.values())

    def summary(self):
        return [*self.parts.items(), ("total", self.total())]


def place_order(cart, region):
    if not cart.lines:
        raise ValueError("an order needs at least one line")
    return Order(price(cart, region))
