"""Base prices of earlier releases, for questions about old invoices."""

from decimal import Decimal

from wholesale import catalog

# Releases, oldest first, with the base prices that changed in each.
CHANGES = (
    ("2024.1", {"bolt-m6": Decimal("0.36"), "saw-500": Decimal("17.50"), "glue-750": Decimal("6.90")}),
    ("2025.1", {"bolt-m6": Decimal("0.38"), "hinge-80": Decimal("3.95")}),
    ("2025.2", {"saw-500": Decimal("18.40")}),
)


def base_price_in(sku, release):
    """The base price of `sku` in `release`: the last change at or before
    it, or today's price if it never changed."""
    names = [name for name, _ in CHANGES]
    if release not in names:
        raise KeyError(f"unknown release {release!r}")
    price = None
    for name, changed in CHANGES:
        if sku in changed:
            price = changed[sku]
        if name == release:
            break
    return price if price is not None else catalog.product(sku).base_price
