# Route: GET /products
"""The first page of product ids; later pages are /products?page=N in the next release."""

from services.catalog import catalog
from services.catalog.handlers.base import respond

PAGE = 20

REQUIRED_SCOPE = "catalog:read"


def handle(request):
    ids = catalog.product_ids()
    return respond({"count": len(ids), "ids": ids[:PAGE]})
