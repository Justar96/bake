"""Orders and their totals."""

from dataclasses import dataclass
from decimal import Decimal

from wholesale import catalog, promotions, shipping, tax, validation
from wholesale.audit import AuditLog
from wholesale.customers import customer as find_customer
from wholesale.money import ZERO, percent_of, round_money
from wholesale.quotes import QuoteCache


@dataclass(frozen=True)
class Line:
    sku: str
    quantity: int


@dataclass(frozen=True)
class Order:
    number: int
    customer: str
    lines: tuple
    promotion: str = ""


@dataclass(frozen=True)
class PricedLine:
    sku: str
    quantity: int
    unit_price: Decimal
    total: Decimal


@dataclass(frozen=True)
class Bill:
    lines: tuple
    subtotal: Decimal
    promotion: Decimal
    shipping: Decimal
    tax_rate: Decimal
    tax: Decimal
    total: Decimal


class OrderService:
    """Prices orders; one service handles many orders and shares its quote cache."""

    def __init__(self, quotes=None, audit=None):
        self.quotes = quotes if quotes is not None else QuoteCache()
        self.audit = audit if audit is not None else AuditLog()

    def price_lines(self, order):
        validation.check_lines(order.lines)
        group = find_customer(order.customer).group
        priced = []
        for line in order.lines:
            unit = self.quotes.unit_price(line.sku, line.quantity, group)
            priced.append(PricedLine(line.sku, line.quantity, unit, round_money(unit * line.quantity)))
        return priced

    def bill(self, order):
        client = find_customer(order.customer)
        lines = self.price_lines(order)
        subtotal = sum((line.total for line in lines), ZERO)
        off = promotions.discount(order.promotion, client.group, subtotal)
        grams = sum(catalog.product(line.sku).grams * line.quantity for line in lines)
        carriage = shipping.cost(client.region, grams, subtotal - off)
        rate = tax.rate_for(client.region)
        taxed = round_money(percent_of(subtotal - off + carriage, rate))
        total = subtotal - off + carriage + taxed
        for what, amount in (("subtotal", subtotal), ("promotion", off), ("shipping", carriage), ("tax", taxed)):
            self.audit.record(order.number, what, amount)
        return Bill(tuple(lines), subtotal, off, carriage, rate, taxed, total)

    def total(self, order):
        return self.bill(order).total
