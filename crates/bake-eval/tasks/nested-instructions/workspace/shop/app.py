"""Routes a request to its handler."""

from dataclasses import dataclass, field

from services.billing.handlers import ROUTES as BILLING
from services.catalog.handlers import ROUTES as CATALOG
from services.orders.handlers import ROUTES as ORDERS
from shop import errors

ROUTES = CATALOG + ORDERS + BILLING


@dataclass(frozen=True)
class Request:
    method: str
    path: str
    params: dict = field(default_factory=dict)


def match(pattern, path):
    """The parameters of `path` under `pattern`, such as {"id": "p0001"} for
    /products/p0001 under /products/{id}; None if it does not match."""
    want = pattern.strip("/").split("/")
    got = path.strip("/").split("/")
    if len(want) != len(got):
        return None
    params = {}
    for expected, actual in zip(want, got):
        if expected.startswith("{") and expected.endswith("}"):
            if not actual:
                return None
            params[expected[1:-1]] = actual
        elif expected != actual:
            return None
    return params


def dispatch(method, path, scopes=()):
    """(status, body) for a request by a caller holding `scopes`."""
    for route_method, pattern, handler in ROUTES:
        params = match(pattern, path)
        if params is None or route_method != method:
            continue
        scope = handler.REQUIRED_SCOPE
        if scope != "public" and scope not in scopes:
            return errors.forbidden(f"{method} {pattern} needs the scope {scope}")
        return handler.handle(Request(method, path, params))
    return errors.not_found(f"no route for {method} {path}")
