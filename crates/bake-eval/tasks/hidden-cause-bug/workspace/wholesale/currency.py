"""Amounts shown in other currencies, for customers who ask for them.

Bills are always in euros; this is only for display, so it rounds half to
even like the bank's converter does, not half up like the invoices.
"""

from decimal import ROUND_HALF_EVEN, Decimal

# Euros per unit of each currency, fixed each quarter by the finance team.
RATES = {"EUR": Decimal(1), "CHF": Decimal("1.0412"), "GBP": Decimal("1.1683"), "SEK": Decimal("0.0871")}


def convert(amount_eur, currency):
    """`amount_eur` in `currency`, rounded half to even to the cent."""
    try:
        rate = RATES[currency]
    except KeyError:
        raise ValueError(f"unknown currency {currency!r}") from None
    return (Decimal(amount_eur) / rate).quantize(Decimal("0.01"), rounding=ROUND_HALF_EVEN)


def show(amount_eur, currency):
    return f"{convert(amount_eur, currency):,.2f} {currency}"
