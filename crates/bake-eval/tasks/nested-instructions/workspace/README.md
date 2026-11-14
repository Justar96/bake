# shop

A read-only API over the shop's catalog, orders, and billing. Each service
in `services/` has handlers registered in its `handlers/__init__.py`;
`shop.app.dispatch(method, path, scopes)` finds the handler for a request
and returns `(status, body)`. A caller needs the handler's scope.

The data in `shop/data/` is a nightly export; `tools/` holds the scripts
that maintain it.
