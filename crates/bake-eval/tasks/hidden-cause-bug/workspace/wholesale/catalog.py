"""The products, their families, and each family's volume discounts."""

from collections import namedtuple
from decimal import Decimal

Product = namedtuple("Product", "sku name family base_price grams")

# Each family's price ladder: the lower bound of every quantity step, and
# the discount of that step in percent. With bounds (1, 10, 50, 100), step 0
# is 1-9 units, step 1 is 10-49, step 2 is 50-99, and step 3 is 100 or more.
FAMILIES = {
    "fasteners": {"bounds": (1, 10, 50, 100), "discounts": (0, 5, 10, 15)},
    "hardware": {"bounds": (1, 5, 20), "discounts": (0, 4, 8)},
    "tools": {"bounds": (1, 3, 12), "discounts": (0, 3, 6)},
    "consumables": {"bounds": (1, 25, 250), "discounts": (0, 6, 12)},
}

_PRODUCTS = (
    ("bolt-m6", "Bolt M6x30", "fasteners", "0.40", 9),
    ("bolt-m8", "Bolt M8x40", "fasteners", "0.65", 18),
    ("nut-m6", "Nut M6", "fasteners", "0.15", 3),
    ("nut-m8", "Nut M8", "fasteners", "0.22", 5),
    ("washer-m6", "Washer M6", "fasteners", "0.05", 1),
    ("screw-4x40", "Wood screw 4x40", "fasteners", "0.08", 4),
    ("hinge-80", "Hinge 80 mm", "hardware", "4.20", 110),
    ("hinge-120", "Hinge 120 mm", "hardware", "6.90", 180),
    ("bracket-l", "Angle bracket", "hardware", "2.50", 90),
    ("handle-128", "Cabinet handle 128 mm", "hardware", "3.35", 75),
    ("latch-s", "Small latch", "hardware", "1.85", 40),
    ("saw-500", "Hand saw 500 mm", "tools", "18.90", 520),
    ("drill-set", "Drill bit set", "tools", "24.50", 640),
    ("square-300", "Try square 300 mm", "tools", "11.20", 310),
    ("clamp-f", "F clamp", "tools", "9.75", 700),
    ("glue-750", "Wood glue 750 g", "consumables", "7.40", 790),
    ("sandpaper-120", "Sandpaper P120", "consumables", "0.35", 12),
    ("tape-50", "Masking tape 50 m", "consumables", "2.10", 160),
)

PRODUCTS = {sku: Product(sku, name, family, Decimal(price), grams) for sku, name, family, price, grams in _PRODUCTS}


def product(sku):
    """The product, or KeyError naming the unknown sku."""
    try:
        return PRODUCTS[sku]
    except KeyError:
        raise KeyError(f"unknown product {sku!r}") from None
