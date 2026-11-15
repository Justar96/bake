"""Shipping charges."""

from shop import config


def bulk_shipping_rate(cart):
    """The per-unit rate: large orders ship in bulk for less."""
    if cart.units() >= config.BULK_SHIPPING_QTY:
        return config.BULK_SHIPPING_RATE
    return config.STANDARD_SHIPPING_RATE


def shipping_charge(cart, discounted_subtotal):
    if discounted_subtotal >= config.FREE_SHIPPING_SUBTOTAL:
        return 0.0
    return cart.units() * bulk_shipping_rate(cart)
