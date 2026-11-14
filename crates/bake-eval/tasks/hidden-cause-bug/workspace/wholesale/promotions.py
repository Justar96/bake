"""Promotion codes, which take a percentage off an order's subtotal."""

from decimal import Decimal

from wholesale.money import percent_of, round_money

# code: (percent off, the customer groups it is for)
PROMOTIONS = {
    "SPRING10": (Decimal(10), ("trade",)),
    "WELCOME5": (Decimal(5), ("retail", "trade")),
}


class PromotionError(Exception):
    pass


def discount(code, group, subtotal):
    """The amount a promotion takes off `subtotal`, rounded to the cent.
    An empty code takes nothing off; a code for other groups is refused."""
    if not code:
        return Decimal("0.00")
    try:
        percent, groups = PROMOTIONS[code.upper()]
    except KeyError:
        raise PromotionError(f"unknown promotion {code!r}") from None
    if group not in groups:
        raise PromotionError(f"{code} is not for {group} customers")
    return round_money(percent_of(subtotal, percent))
