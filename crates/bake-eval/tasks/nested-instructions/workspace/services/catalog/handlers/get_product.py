# Route: GET /products/{id}
"""One product."""

from services.catalog import catalog
from services.catalog.handlers.base import error, respond

REQUIRED_SCOPE = "catalog:read"


def handle(request):
    product = catalog.product(request.params["id"])
    if product is None:
        return error(404, "product_not_found")
    return respond(product)
