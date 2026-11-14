"""GET /orders/{id}/lines: the lines of one order."""

from shop import errors, store

REQUIRED_SCOPE = "orders:read"


def handle(request):
    order = store.orders().get(request.params["id"])
    if order is None:
        return errors.not_found(f"no order {request.params['id']}")
    return 200, order["lines"]
