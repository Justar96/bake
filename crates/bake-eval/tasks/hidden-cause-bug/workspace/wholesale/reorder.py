"""Reorder suggestions: what to buy so the shelf covers the coming weeks."""

import math

from wholesale import catalog


def weekly_demand(orders, weeks):
    """Units sold per week of each sku over `orders`, which span `weeks` weeks."""
    if weeks <= 0:
        raise ValueError("weeks must be positive")
    sold = {}
    for order in orders:
        for line in order.lines:
            sold[line.sku] = sold.get(line.sku, 0) + line.quantity
    return {sku: units / weeks for sku, units in sorted(sold.items())}


def suggestions(inventory, demand, cover_weeks=4, pack_sizes=None):
    """(sku, units to order) for every sku whose available stock covers fewer
    than `cover_weeks` weeks, rounded up to whole packs."""
    pack_sizes = pack_sizes or {}
    out = []
    for sku in sorted(catalog.PRODUCTS):
        need = demand.get(sku, 0) * cover_weeks - inventory.available(sku)
        if need <= 0:
            continue
        pack = pack_sizes.get(sku, 1)
        packs = -(-math.ceil(need) // pack)
        out.append((sku, packs * pack))
    return out
