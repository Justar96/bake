# Catalog handlers

These rules apply to everything in `services/catalog/handlers/` and take
precedence over the root AGENTS.md here.

1. One handler a module. Name the module after the method and the literal
   path segments, lowercase, joined by `_`, leaving out parameters and
   making a segment singular when a parameter follows it:
   `GET /categories/{id}/products` is `get_category_products.py`.
2. A module starts with the line `# Route: <METHOD> <path>`, then defines
   `REQUIRED_SCOPE` and one function, `handle(request)`. Every GET handler
   uses the scope `catalog:read`, except `/health`, which is `public`.
3. Handlers answer with `respond(...)` and `error(...)` from `base.py`,
   never with `shop/errors.py`. A record that does not exist is
   `error(404, "<resource>_not_found")`, with the resource in the singular.
4. Handlers read data only through `catalog.py` and `stock.py`, never
   `shop/store.py`. Stock is always reported as `stock.units_on_hand()`.
5. Register every handler in `ROUTES` in `__init__.py`, keeping the list
   sorted by path. Record each change in `CHANGES.md` under
   `## Unreleased`: a new route under `### Added`, a change to an
   existing route under `### Changed`, as `- <METHOD> <path>: <text>`.
6. Each handler module has its own test module,
   `tests/catalog/test_<handler module>.py`, which makes its requests with
   `catalog_request()` from `tests/catalog/helpers.py`.
