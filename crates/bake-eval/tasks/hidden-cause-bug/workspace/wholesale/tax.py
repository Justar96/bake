"""Tax: the country's VAT plus a regional levy where one applies."""

from decimal import Decimal

from wholesale.regions import country_of, normalize

VAT = {"AT": Decimal(20), "DE": Decimal(19), "FR": Decimal(20), "NL": Decimal(21), "BE": Decimal(21)}

# Regional levies in percent, on top of the VAT, by normalized region code.
LEVIES = {"DE-BY": Decimal("1.5"), "AT-9": Decimal("2"), "FR-IDF": Decimal("0.5"), "BE-BRU": Decimal("1")}


class TaxError(Exception):
    pass


def rate_for(region):
    """The tax rate of a region in percent."""
    country = country_of(region)
    if country not in VAT:
        raise TaxError(f"we do not ship to {country}")
    return VAT[country] + LEVIES.get(normalize(region), Decimal(0))
