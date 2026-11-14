"""Puts the parts of a price together."""

from shop.discounts import order_discount
from shop.shipping import shipping_charge
from shop.tax import tax


def price(cart, region):
    subtotal = cart.subtotal()
    discount = order_discount(cart)
    discounted = subtotal - discount
    shipping = shipping_charge(cart, discounted)
    return {
        "subtotal": subtotal,
        "discount": -discount,
        "shipping": shipping,
        "tax": tax(region, discounted + shipping),
    }
