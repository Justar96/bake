"""GET /orders: the ids of the open orders."""

from shop import store

REQUIRED_SCOPE = "orders:read"


def handle(request):
    return 200, sorted(key for key, order in store.orders().items() if order["status"] == "open")
