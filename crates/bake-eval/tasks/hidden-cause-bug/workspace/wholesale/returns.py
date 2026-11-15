"""Credit notes for returned goods.

A return is refunded at the unit price the customer paid on the original
bill, not today's price, with the tax rate of that bill. Shipping is not
refunded, and a promotion is taken back in proportion.
"""

from dataclasses import dataclass
from decimal import Decimal

from wholesale.money import ZERO, percent_of, round_money


class ReturnError(Exception):
    pass


@dataclass(frozen=True)
class CreditNote:
    order_number: int
    lines: tuple
    net: Decimal
    tax: Decimal
    total: Decimal


def credit_note(order_number, bill, returned):
    """A credit note for `returned`, {sku: units}, against `bill`."""
    paid = {}
    for line in bill.lines:
        paid.setdefault(line.sku, []).append(line)
    lines = []
    for sku, units in sorted(returned.items()):
        bought = paid.get(sku)
        if not bought:
            raise ReturnError(f"order {order_number} has no {sku}")
        if units > sum(line.quantity for line in bought):
            raise ReturnError(f"order {order_number} has fewer than {units} of {sku}")
        # Refund the cheapest units first: they are the ones bought in the
        # largest line, which is how the warehouse books returns.
        remaining = units
        for line in sorted(bought, key=lambda line: line.unit_price):
            take = min(remaining, line.quantity)
            if take:
                lines.append((sku, take, round_money(line.unit_price * take)))
            remaining -= take
    net = sum((amount for _, _, amount in lines), ZERO)
    if bill.subtotal and bill.promotion:
        net -= round_money(bill.promotion * net / bill.subtotal)
    tax = round_money(percent_of(net, bill.tax_rate))
    return CreditNote(order_number, tuple(lines), net, tax, net + tax)
