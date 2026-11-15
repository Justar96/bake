"""Sales reports."""

from decimal import Decimal

from wholesale import catalog

from wholesale.customers import customer as find_customer
from wholesale.ladders import ladder, ladder_of
from wholesale.regions import normalize


def step_summary(orders):
    """Units sold on each ladder step, by family and step label, in family
    and step order; steps with no sales are left out."""
    units = {}
    for order in orders:
        for line in order.lines:
            steps = ladder_of(line.sku)
            key = (steps.family, steps.step_of(line.quantity))
            units[key] = units.get(key, 0) + line.quantity
    return {f"{family} {ladder(family).label(step)}": count for (family, step), count in sorted(units.items())}


def orders_by_region(orders):
    """How many orders went to each normalized region code, sorted by code."""
    counts = {}
    for order in orders:
        region = normalize(find_customer(order.customer).region)
        counts[region] = counts.get(region, 0) + 1
    return dict(sorted(counts.items()))


def revenue_by_family(service, orders):
    """The line totals of `orders`, by product family, sorted by family."""
    revenue = {}
    for order in orders:
        for line in service.price_lines(order):
            family = catalog.product(line.sku).family
            revenue[family] = revenue.get(family, Decimal("0.00")) + line.total
    return dict(sorted(revenue.items()))


def top_products(orders, count=5):
    """The `count` skus sold in the most units, most first, then by sku."""
    units = {}
    for order in orders:
        for line in order.lines:
            units[line.sku] = units.get(line.sku, 0) + line.quantity
    return sorted(units.items(), key=lambda item: (-item[1], item[0]))[:count]
