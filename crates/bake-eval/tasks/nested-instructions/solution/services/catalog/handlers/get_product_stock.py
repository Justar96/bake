# Route: GET /products/{id}/stock
"""The units of one product that can be sold now."""

from services.catalog import catalog, stock
from services.catalog.handlers.base import error, respond

REQUIRED_SCOPE = "catalog:read"


def handle(request):
    product_id = request.params["id"]
    if catalog.product(product_id) is None:
        return error(404, "product_not_found")
    return respond({"id": product_id, "in_stock": stock.units_on_hand(product_id)})
