"""Checks on an order before it is priced."""

from wholesale import catalog


class OrderError(Exception):
    pass


def check_lines(lines):
    """Raises OrderError for an empty order, an unknown product, or a line
    without units. Repeated products are allowed: each line is priced alone."""
    if not lines:
        raise OrderError("an order needs at least one line")
    for number, line in enumerate(lines, 1):
        if line.sku not in catalog.PRODUCTS:
            raise OrderError(f"line {number}: unknown product {line.sku!r}")
        if line.quantity <= 0:
            raise OrderError(f"line {number}: {line.quantity} units")
