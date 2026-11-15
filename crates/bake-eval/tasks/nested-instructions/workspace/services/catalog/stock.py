"""Stock levels as the catalog reports them."""

from shop import store


def units_on_hand(product_id):
    """Units that can be sold now: those on the shelf less those reserved for
    open orders, never below zero. A product with no stock record has none."""
    record = store.stock().get(product_id)
    if record is None:
        return 0
    return max(record["shelf"] - record["reserved"], 0)


def bin_of(product_id):
    """Where the product is shelved, or None."""
    record = store.stock().get(product_id)
    return record["bin"] if record else None
