"""Quantity steps, shared by price ladders and reports.

A table of bounds is an increasing tuple of lower bounds. With bounds
`(1, 10, 50, 100)`, step 0 holds 1 to 9 units, step 1 holds 10 to 49, step 2
holds 50 to 99, and step 3 holds 100 units or more.
"""


def tier_index(bounds, quantity):
    """The index of the step that `quantity` falls in.

    Raises ValueError for a quantity below the first bound.
    """
    if quantity < bounds[0]:
        raise ValueError(f"quantity {quantity} is below the first step")
    index = 0
    for position, bound in enumerate(bounds):
        if quantity > bound:
            index = position
    return index


def tier_label(bounds, index):
    """A step as people write it: "10-49", or "100+" for the last step."""
    low = bounds[index]
    if index == len(bounds) - 1:
        return f"{low}+"
    return f"{low}-{bounds[index + 1] - 1}"
