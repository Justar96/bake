# Route: GET /products/{id}
"""One product, with the units that can be sold now."""

from services.catalog import catalog, stock
from services.catalog.handlers.base import error, respond

REQUIRED_SCOPE = "catalog:read"


def handle(request):
    product = catalog.product(request.params["id"])
    if product is None:
        return error(404, "product_not_found")
    return respond({**product, "in_stock": stock.units_on_hand(product["id"])})
