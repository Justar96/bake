"""Tunable business rules."""

from shop.units import DOZEN, SCORE

# A line with at least this many units of one item gets the volume break.
VOLUME_BREAK_QTY = DOZEN
VOLUME_BREAK_RATE = 0.08

CLEARANCE_RATE = 0.25

# An order with at least this many units in total ships at the bulk rate.
BULK_SHIPPING_QTY = SCORE
BULK_SHIPPING_RATE = 0.35
STANDARD_SHIPPING_RATE = 0.6
FREE_SHIPPING_SUBTOTAL = 150.0

LOYALTY_RATE_PER_YEAR = 0.01
LOYALTY_RATE_CAP = 0.05

TAX_RATES = {"north": 0.07, "south": 0.055, "export": 0.0}
