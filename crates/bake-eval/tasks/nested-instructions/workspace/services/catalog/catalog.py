"""Products and categories as the catalog handlers see them."""

from shop import store


def product(product_id):
    """The product's public fields, or None if there is no such product."""
    record = store.products().get(product_id)
    if record is None:
        return None
    return {"id": product_id, "name": record["name"], "category": record["category"], "price": record["price"]}


def product_ids():
    return sorted(store.products())


def categories():
    return sorted({record["category"] for record in store.products().values()})


def in_category(category):
    return [product_id for product_id in product_ids() if store.products()[product_id]["category"] == category]


def search(term):
    term = term.lower()
    return [product_id for product_id in product_ids() if term in store.products()[product_id]["name"].lower()]
