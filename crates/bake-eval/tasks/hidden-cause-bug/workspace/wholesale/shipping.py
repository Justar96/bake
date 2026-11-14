"""Shipping charges by destination country and weight."""

from decimal import Decimal

from wholesale.regions import country_of

# Shipping is priced per country only; subdivisions do not matter here,
# so country_of() is all we need.
ZONES = {"DE": 1, "AT": 1, "NL": 1, "BE": 2, "FR": 2}

# Per zone: (weight limit in grams, charge), the first limit the parcel fits wins.
BANDS = {
    1: ((1000, Decimal("4.90")), (5000, Decimal("7.90")), (20000, Decimal("12.50"))),
    2: ((1000, Decimal("8.90")), (5000, Decimal("12.90")), (20000, Decimal("19.50"))),
}
HEAVY_PER_KG = Decimal("0.80")
FREE_FROM = Decimal("500.00")


def cost(region, grams, subtotal):
    """The charge for a parcel of `grams` to `region`; free from FREE_FROM."""
    if subtotal >= FREE_FROM:
        return Decimal("0.00")
    zone = ZONES[country_of(region)]
    for limit, charge in BANDS[zone]:
        if grams <= limit:
            return charge
    heaviest, charge = BANDS[zone][-1]
    extra_kg = -(-(grams - heaviest) // 1000)  # whole kilograms, rounded up
    return charge + HEAVY_PER_KG * extra_kg
