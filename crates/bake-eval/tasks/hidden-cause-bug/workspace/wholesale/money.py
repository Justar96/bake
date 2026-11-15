"""Rounding money the way the accountants want it."""

from decimal import ROUND_HALF_UP, Decimal

CENT = Decimal("0.01")
ZERO = Decimal("0.00")


def round_money(amount):
    """Rounds to the cent, halves up (0.005 -> 0.01), as invoices require."""
    return Decimal(amount).quantize(CENT, rounding=ROUND_HALF_UP)


def percent_of(amount, percent):
    """`percent` per cent of `amount`, not rounded."""
    return Decimal(amount) * Decimal(percent) / Decimal(100)


def format_money(amount):
    """Two decimals and a thousands separator: 1,234.50."""
    return f"{round_money(amount):,.2f}"
