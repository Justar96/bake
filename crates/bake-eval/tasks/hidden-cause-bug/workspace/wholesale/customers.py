"""Customer accounts, as the CRM exports them."""

from collections import namedtuple

Customer = namedtuple("Customer", "id name group region")

# The CRM keeps region codes as people typed them; regions.normalize()
# turns them into canonical codes.
_CUSTOMERS = (
    ("c01", "ACME Werkstatt", "trade", "de"),
    ("c02", "Bauhof Huber", "trade", "de-by"),
    ("c03", "Tischlerei Gruber", "trade", "AT_9"),
    ("c04", "Van Dijk Bouw", "trade", "nl"),
    ("c05", "Atelier Martin", "retail", "fr idf"),
    ("c06", "Brico Lyon", "retail", "fr"),
    ("c07", "Hobbykeller Meier", "retail", " de-BY "),
    ("c08", "Schreinerei Wolf", "retail", "DE"),
    ("c09", "Maison Dupont", "trade", "be-bru"),
    ("c10", "Klusbedrijf Smit", "retail", "NL"),
    ("c11", "Holzbau Steiner", "trade", "at"),
    ("c12", "Quincaillerie Roux", "trade", "be"),
)

CUSTOMERS = {key: Customer(key, name, group, region) for key, name, group, region in _CUSTOMERS}


def customer(key):
    try:
        return CUSTOMERS[key]
    except KeyError:
        raise KeyError(f"unknown customer {key!r}") from None
