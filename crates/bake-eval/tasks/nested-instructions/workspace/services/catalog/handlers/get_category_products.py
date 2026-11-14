# Route: GET /categories/{id}/products
"""The ids of one category's products."""

from services.catalog import catalog
from services.catalog.handlers.base import error, respond

REQUIRED_SCOPE = "catalog:read"


def handle(request):
    category = request.params["id"]
    if category not in catalog.categories():
        return error(404, "category_not_found")
    return respond(catalog.in_category(category))
