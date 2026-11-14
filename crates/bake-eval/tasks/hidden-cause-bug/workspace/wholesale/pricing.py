"""Unit prices: the base price less the volume and customer group discounts."""

from wholesale import catalog
from wholesale.money import percent_of

# Trade customers get this much off every price, in percent, after the volume discount.
GROUP_DISCOUNTS = {"retail": 0, "trade": 5}


class PricingError(Exception):
    pass


def volume_discount(sku, quantity):
    """The volume discount of a line of `quantity` units of `sku`, in percent."""
    if quantity < 1:
        raise PricingError(f"a line needs at least one unit, not {quantity}")
    table = catalog.FAMILIES[catalog.product(sku).family]
    # Walk the steps from the top: the first bound the quantity reaches wins.
    for bound, percent in reversed(list(zip(table["bounds"], table["discounts"]))):
        if quantity >= bound:
            return percent
    raise PricingError(f"no step for {quantity}")  # unreachable: bounds start at 1


def unit_price(sku, quantity, group="retail"):
    """The exact unit price of `sku` in a line of `quantity` units for a
    customer of `group`; not rounded."""
    try:
        base = catalog.product(sku).base_price
    except KeyError as error:
        raise PricingError(str(error)) from None
    if group not in GROUP_DISCOUNTS:
        raise PricingError(f"unknown customer group {group!r}")
    after_volume = base - percent_of(base, volume_discount(sku, quantity))
    return after_volume - percent_of(after_volume, GROUP_DISCOUNTS[group])
