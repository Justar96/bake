"""Discounts for returning customers."""

from shop import config


def loyalty_rate(years):
    return min(config.LOYALTY_RATE_CAP, max(0, years) * config.LOYALTY_RATE_PER_YEAR)


def loyalty_discount(cart, discounted_subtotal):
    """Taken off what is left after the line discounts."""
    return discounted_subtotal * loyalty_rate(cart.customer_years)
