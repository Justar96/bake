"""Price ladders: a family's quantity steps and their discounts."""

import functools

from wholesale import catalog
from wholesale.tiers import tier_index, tier_label


class Ladder:
    def __init__(self, family, bounds, discounts):
        if len(bounds) != len(discounts):
            raise ValueError(f"{family}: {len(bounds)} bounds but {len(discounts)} discounts")
        if list(bounds) != sorted(set(bounds)):
            raise ValueError(f"{family}: the bounds must increase")
        self.family = family
        self.bounds = tuple(bounds)
        self.discounts = tuple(discounts)

    def step_of(self, quantity):
        """The step a line of `quantity` units is on."""
        return tier_index(self.bounds, quantity)

    def label(self, step):
        return tier_label(self.bounds, step)

    def __repr__(self):
        return f"Ladder({self.family!r}, {self.bounds!r}, {self.discounts!r})"


@functools.lru_cache(maxsize=None)
def ladder(family):
    table = catalog.FAMILIES[family]
    return Ladder(family, table["bounds"], table["discounts"])


def ladder_of(sku):
    return ladder(catalog.product(sku).family)
