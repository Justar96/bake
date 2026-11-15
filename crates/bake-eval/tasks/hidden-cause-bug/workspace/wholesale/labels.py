"""Shipping labels."""

from wholesale import catalog
from wholesale.customers import customer as find_customer
from wholesale.regions import country_of

COUNTRY_NAMES = {"AT": "AUSTRIA", "BE": "BELGIUM", "DE": "GERMANY", "FR": "FRANCE", "NL": "NETHERLANDS"}


def label(order):
    client = find_customer(order.customer)
    grams = sum(catalog.product(line.sku).grams * line.quantity for line in order.lines)
    kilograms = f"{grams / 1000:.1f}"
    parcels = max(1, -(-grams // 20000))  # one parcel per started 20 kg
    return "\n".join([
        client.name.upper(),
        COUNTRY_NAMES[country_of(client.region)],
        f"ORDER {order.number}  {kilograms} KG  {parcels} PARCEL{'S' if parcels > 1 else ''}",
    ]) + "\n"
