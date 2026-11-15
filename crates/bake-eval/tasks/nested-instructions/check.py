"""Run in the workspace: both endpoints work and follow services/catalog/handlers/AGENTS.md."""

import json
import os
import re
import sys

sys.path.insert(0, os.getcwd())
TEMPLATE = os.path.join(os.environ["BAKE_EVAL_TASK_DIR"], "workspace")
HANDLERS = "services/catalog/handlers"


def read(path, root="."):
    with open(os.path.join(root, path), encoding="utf-8") as handle:
        return handle.read()


def fail(message):
    print(message)
    sys.exit(1)


new_module = f"{HANDLERS}/get_product_stock.py"
if not os.path.isfile(new_module):
    fail(f"no {new_module}: catalog handler modules are named after the method and the path")
for path, route in ((new_module, "GET /products/{id}/stock"), (f"{HANDLERS}/get_product.py", "GET /products/{id}")):
    source = read(path)
    if source.splitlines()[0] != f"# Route: {route}":
        fail(f"{path} does not start with '# Route: {route}'")
    if re.search(r"^\s*(from\s+shop(\.\w+)?\s+import|import\s+shop)", source, re.M):
        fail(f"{path} imports from shop/; catalog handlers use base.py, catalog.py, and stock.py")

from services.catalog import stock  # noqa: E402
from services.catalog.handlers import ROUTES, get_product, get_product_stock  # noqa: E402
from shop.app import dispatch  # noqa: E402

if get_product_stock.REQUIRED_SCOPE != "catalog:read":
    fail(f"the stock handler's scope is {get_product_stock.REQUIRED_SCOPE!r}, not 'catalog:read'")
if ("GET", "/products/{id}/stock", get_product_stock) not in ROUTES:
    fail("the catalog ROUTES do not register GET /products/{id}/stock with get_product_stock")
paths = [path for _, path, _ in ROUTES]
if paths != sorted(paths):
    fail(f"the catalog ROUTES are not sorted by path: {paths}")

with open("shop/data/products.json", encoding="utf-8") as handle:
    products = json.load(handle)
READ = ("catalog:read",)
for product_id in ("p0007", "p0042", "p0099", "p0001", "p0500", "p1200"):
    units = stock.units_on_hand(product_id)
    got = dispatch("GET", f"/products/{product_id}/stock", READ)
    if got != (200, {"id": product_id, "in_stock": units}):
        fail(f"GET /products/{product_id}/stock gave {got!r}, not (200, {{'id': {product_id!r}, 'in_stock': {units}}})")
    record = products[product_id]
    expected = {"id": product_id, "name": record["name"], "category": record["category"],
                "price": record["price"], "in_stock": units}
    got = dispatch("GET", f"/products/{product_id}", READ)
    if got != (200, expected):
        fail(f"GET /products/{product_id} gave {got!r}, not (200, {expected!r})")
for path in ("/products/p9999/stock", "/products/p9999"):
    got = dispatch("GET", path, READ)
    if got != (404, {"error": "product_not_found"}):
        fail(f"GET {path} gave {got!r}, not (404, {{'error': 'product_not_found'}})")
if dispatch("GET", "/products/p0007/stock", ("orders:read",))[0] != 403:
    fail("GET /products/{id}/stock does not need the scope catalog:read")

# CHANGES.md: the original lines, one more under ### Added and one under
# ### Changed, both in ## Unreleased.
changes = read(f"{HANDLERS}/CHANGES.md").splitlines()
original = read(f"{HANDLERS}/CHANGES.md", TEMPLATE).splitlines()
added = re.compile(r"^- GET /products/\{id\}/stock: \S")
changed = re.compile(r"^- GET /products/\{id\}: \S")
new_lines = [n for n, line in enumerate(changes) if line not in original]
kinds = sorted("added" if added.match(changes[n]) else "changed" if changed.match(changes[n]) else "other"
               for n in new_lines)
if kinds != ["added", "changed"] or [line for line in changes if line.strip() and line not in [changes[n] for n in new_lines]] != [line for line in original if line.strip()]:
    fail(f"{HANDLERS}/CHANGES.md needs exactly one new line for each route and no other change")
for number in new_lines:
    headings = [line for line in changes[:number] if line.startswith("#")]
    want = "### Added" if added.match(changes[number]) else "### Changed"
    if headings[-1] != want or [h for h in headings if h.startswith("## ")][-1] != "## Unreleased":
        fail(f"CHANGES.md: {changes[number]!r} is not under ## Unreleased, {want}")

for module, needle in (("test_get_product_stock.py", "/stock"), ("test_get_product.py", "in_stock")):
    path = os.path.join("tests", "catalog", module)
    if not os.path.isfile(path):
        fail(f"no {path}: each catalog handler has its own test module")
    text = read(path)
    if "catalog_request" not in text or needle not in text:
        fail(f"{path} does not test {needle} with catalog_request()")
