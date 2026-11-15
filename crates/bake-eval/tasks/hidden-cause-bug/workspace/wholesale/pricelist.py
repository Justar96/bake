"""The printed price list: every product's unit price on each ladder step."""

from wholesale import catalog, pricing
from wholesale.ladders import ladder


def rows(group="retail"):
    """(family, sku, name, [(step label, unit price)]) for every product, by family then sku."""
    out = []
    for family in sorted(catalog.FAMILIES):
        steps = ladder(family)
        for sku in sorted(sku for sku, product in catalog.PRODUCTS.items() if product.family == family):
            prices = []
            for step, bound in enumerate(steps.bounds):
                # The price on a step is the price of its smallest quantity.
                prices.append((steps.label(step), pricing.unit_price(sku, bound, group)))
            out.append((family, sku, catalog.product(sku).name, prices))
    return out


def render(group="retail"):
    lines = [f"Price list ({group})"]
    family = None
    for row_family, sku, name, prices in rows(group):
        if row_family != family:
            family = row_family
            lines += ["", family.capitalize()]
        cells = "  ".join(f"{label:>7} {price:>8.4f}" for label, price in prices)
        lines.append(f"  {sku:<14} {name:<24} {cells}")
    return "\n".join(lines) + "\n"
