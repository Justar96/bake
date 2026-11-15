# Route: GET /health
"""Liveness."""

from services.catalog.handlers.base import respond

REQUIRED_SCOPE = "public"


def handle(request):
    return respond({"status": "ok"})
