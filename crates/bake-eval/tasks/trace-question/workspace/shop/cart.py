"""Carts and their lines."""

from dataclasses import dataclass, field


@dataclass
class Line:
    item: str
    unit_price: float
    quantity: int
    clearance: bool = False

    def subtotal(self):
        return self.unit_price * self.quantity


@dataclass
class Cart:
    customer_years: int = 0
    lines: list = field(default_factory=list)

    def add(self, item, unit_price, quantity, clearance=False):
        if quantity <= 0:
            raise ValueError("quantity must be positive")
        self.lines.append(Line(item, unit_price, quantity, clearance))

    def units(self):
        return sum(line.quantity for line in self.lines)

    def subtotal(self):
        return sum(line.subtotal() for line in self.lines)
