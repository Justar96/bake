"""Margins: what the shop keeps of each line after the cost of the goods."""

from decimal import Decimal

from wholesale.money import ZERO, round_money

# What each product costs the shop, by family, as a share of the base price.
COST_SHARE = {"fasteners": Decimal("0.55"), "hardware": Decimal("0.62"), "tools": Decimal("0.70"),
              "consumables": Decimal("0.48")}


def line_cost(product, quantity):
    return round_money(product.base_price * COST_SHARE[product.family] * quantity)


def margins(bill, products):
    """(sku, revenue, cost, margin) for each line of `bill`; `products` maps
    a sku to its catalog record."""
    out = []
    for line in bill.lines:
        cost = line_cost(products[line.sku], line.quantity)
        out.append((line.sku, line.total, cost, line.total - cost))
    return out


def margin_rate(bill, products):
    """The bill's margin as a share of its revenue, before promotions; 0 for an empty bill."""
    revenue = sum((line.total for line in bill.lines), ZERO)
    if not revenue:
        return Decimal(0)
    kept = sum((margin for _, _, _, margin in margins(bill, products)), ZERO)
    return (kept / revenue).quantize(Decimal("0.0001"))


def below_floor(bill, products, floor=Decimal("0.15")):
    """The skus of lines whose margin is below `floor` of their revenue."""
    return [sku for sku, revenue, _, margin in margins(bill, products) if revenue and margin / revenue < floor]
