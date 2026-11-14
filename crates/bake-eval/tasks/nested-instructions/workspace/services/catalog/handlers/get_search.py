# Route: GET /search/{term}
"""The products whose name contains the term."""

from services.catalog import catalog
from services.catalog.handlers.base import respond

REQUIRED_SCOPE = "catalog:read"


def handle(request):
    return respond(catalog.search(request.params["term"]))
