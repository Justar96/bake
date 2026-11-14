"""Line and order discounts."""

from shop import config
from shop.loyalty import loyalty_discount


def _apply_volume_break(line):
    """Many units of one item are cheaper per unit."""
    if line.quantity >= config.VOLUME_BREAK_QTY:
        return line.subtotal() * config.VOLUME_BREAK_RATE
    return 0.0


def _apply_clearance(line):
    """Clearance items are marked down whatever the quantity."""
    if line.clearance:
        return line.subtotal() * config.CLEARANCE_RATE
    return 0.0


LINE_RULES = (_apply_volume_break, _apply_clearance)


def line_discount(line):
    return sum(rule(line) for rule in LINE_RULES)


def order_discount(cart):
    lines = sum(line_discount(line) for line in cart.lines)
    return lines + loyalty_discount(cart, cart.subtotal() - lines)
