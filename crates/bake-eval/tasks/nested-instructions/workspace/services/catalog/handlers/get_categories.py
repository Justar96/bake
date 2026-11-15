# Route: GET /categories
"""The categories, sorted."""

from services.catalog import catalog
from services.catalog.handlers.base import respond

REQUIRED_SCOPE = "catalog:read"


def handle(request):
    return respond(catalog.categories())
