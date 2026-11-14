"""Order rules the sales team agreed with the warehouse."""

from decimal import Decimal

from wholesale.customers import customer as find_customer

# The smallest subtotal an order may have, by customer group.
MINIMUM_ORDER = {"retail": Decimal("0.00"), "trade": Decimal("25.00")}

# The most lines one order may hold; larger orders are split by the sales team.
MAX_LINES = 40

# Products that only ship in whole boxes of this many units.
BOX_SIZES = {"washer-m6": 10, "screw-4x40": 50, "sandpaper-120": 25}


class RuleError(Exception):
    pass


def problems(order, subtotal):
    """Every rule `order` breaks, as text, in a fixed order; empty if none."""
    found = []
    group = find_customer(order.customer).group
    if subtotal < MINIMUM_ORDER[group]:
        found.append(f"the subtotal {subtotal} is below the {group} minimum of {MINIMUM_ORDER[group]}")
    if len(order.lines) > MAX_LINES:
        found.append(f"{len(order.lines)} lines; split orders of more than {MAX_LINES}")
    for number, line in enumerate(order.lines, 1):
        box = BOX_SIZES.get(line.sku)
        if box and line.quantity % box:
            found.append(f"line {number}: {line.sku} ships in boxes of {box}, not {line.quantity} units")
    return found


def enforce(order, subtotal):
    found = problems(order, subtotal)
    if found:
        raise RuleError("; ".join(found))
