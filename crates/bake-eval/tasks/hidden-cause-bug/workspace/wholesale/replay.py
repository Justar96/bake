"""Reads a day of orders and the totals billed for them, for replays."""

import csv
from decimal import Decimal

from wholesale.orders import Line, Order


def read_orders(path):
    """Orders from a CSV of order, customer, promotion, sku, quantity rows, by number."""
    lines = {}
    heads = {}
    with open(path, newline="", encoding="utf-8") as handle:
        for row in csv.DictReader(handle):
            number = int(row["order"])
            heads.setdefault(number, (row["customer"], row["promotion"]))
            lines.setdefault(number, []).append(Line(row["sku"], int(row["quantity"])))
    return [Order(number, heads[number][0], tuple(lines[number]), heads[number][1]) for number in sorted(lines)]


def read_billed(path):
    with open(path, newline="", encoding="utf-8") as handle:
        return {int(row["order"]): Decimal(row["total"]) for row in csv.DictReader(handle)}


def replay(service, orders, billed):
    """The orders whose total differs from what was billed, as text."""
    wrong = []
    for order in orders:
        total = service.total(order)
        if total != billed[order.number]:
            wrong.append(f"order {order.number}: {total}, billed {billed[order.number]}")
    return wrong
