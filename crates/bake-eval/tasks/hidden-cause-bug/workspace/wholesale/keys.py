"""Cache keys for quotes.

A unit price depends on the product, the customer group, and the ladder step
of the quantity, so two quantities on the same step share a key.
"""

from wholesale.ladders import ladder_of


def quote_key(sku, quantity, group):
    return (sku, group, ladder_of(sku).step_of(quantity))
