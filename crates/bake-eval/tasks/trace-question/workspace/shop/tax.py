"""Sales tax by region."""

from shop import config


def tax(region, taxable):
    try:
        return taxable * config.TAX_RATES[region]
    except KeyError:
        raise ValueError(f"unknown region {region!r}") from None
