"""Run in the workspace: both causes are fixed where they are, and the cache,
prices, rounding, shipping, and the rest behave as before."""

import os
import sys
from decimal import Decimal

sys.path.insert(0, os.getcwd())

from wholesale import catalog, credit, currency, pricing, promotions, shipping, tax  # noqa: E402
from wholesale.exports import export_bills  # noqa: E402
from wholesale.invoice import render_invoice  # noqa: E402
from wholesale.ladders import ladder  # noqa: E402
from wholesale.money import round_money  # noqa: E402
from wholesale.orders import Line, Order, OrderService  # noqa: E402
from wholesale.regions import country_of, normalize  # noqa: E402
from wholesale.replay import read_billed, read_orders, replay  # noqa: E402
from wholesale.reports import orders_by_region, step_summary  # noqa: E402
from wholesale.tiers import tier_index, tier_label  # noqa: E402

TASK = os.environ["BAKE_EVAL_TASK_DIR"]


def expected_step(bounds, quantity):
    return max(index for index, bound in enumerate(bounds) if quantity >= bound)


# The shared step helper puts every quantity, boundaries included, on its step.
for bounds in [tuple(table["bounds"]) for table in catalog.FAMILIES.values()] + [(5, 20), (1,), (2, 3, 4, 1000)]:
    for quantity in range(bounds[0], 1200):
        got = tier_index(bounds, quantity)
        assert got == expected_step(bounds, quantity), (bounds, quantity, got)
    try:
        tier_index(bounds, bounds[0] - 1)
    except ValueError:
        pass
    else:
        raise AssertionError(f"tier_index({bounds}, {bounds[0] - 1}) did not raise ValueError")
assert [tier_label((1, 10, 50, 100), i) for i in range(4)] == ["1-9", "10-49", "50-99", "100+"]
assert [ladder("tools").step_of(q) for q in (1, 2, 3, 11, 12)] == [0, 0, 1, 1, 2]

# Region codes keep their subdivision, whatever its length.
cases = {"de-by": "DE-BY", " de-BY ": "DE-BY", "AT_9": "AT-9", "fr idf": "FR-IDF", "be-bru": "BE-BRU",
         "de": "DE", " NL ": "NL", "nl_zh": "NL-ZH", "fr-": "FR"}
for code, canonical in cases.items():
    assert normalize(code) == canonical, (code, normalize(code), canonical)
    assert country_of(code) == canonical[:2], code
for bad in ("x", "123-ab", "", "deu-by"):
    try:
        normalize(bad)
    except ValueError:
        continue
    raise AssertionError(f"normalize({bad!r}) did not raise ValueError")
rates = {"de-by": "20.5", "AT_9": "22", "fr idf": "20.5", "be-bru": "22", "de": "19", "at": "20", "nl": "21"}
for code, rate in rates.items():
    assert tax.rate_for(code) == Decimal(rate), (code, tax.rate_for(code))

# Reports that use both helpers.
orders = [
    Order(1, "c02", (Line("bolt-m6", 9), Line("bolt-m6", 10), Line("nut-m6", 50))),
    Order(2, "c03", (Line("hinge-80", 20), Line("hinge-80", 4), Line("saw-500", 3), Line("glue-750", 25))),
    Order(3, "c07", (Line("tape-50", 250),)),
]
assert step_summary(orders) == {
    "consumables 25-249": 25, "consumables 250+": 250, "fasteners 1-9": 9, "fasteners 10-49": 10,
    "fasteners 50-99": 50, "hardware 1-4": 4, "hardware 20+": 20, "tools 3-11": 3,
}, step_summary(orders)
assert orders_by_region(orders) == {"AT-9": 1, "DE-BY": 2}, orders_by_region(orders)

# Prices, promotions, rounding, and shipping are unchanged.
assert [pricing.volume_discount("bolt-m6", q) for q in (1, 9, 10, 49, 50, 99, 100)] == [0, 0, 5, 5, 10, 10, 15]
assert pricing.unit_price("washer-m6", 10) == Decimal("0.0475")
assert pricing.unit_price("hinge-80", 5, "trade") == Decimal("3.8304")
assert round_money(Decimal("0.005")) == Decimal("0.01")
assert promotions.discount("spring10", "trade", Decimal("35.91")) == Decimal("3.59")
assert shipping.cost("de-by", 23500, Decimal(10)) == Decimal("15.70")
assert shipping.cost("fr idf", 900, Decimal(10)) == Decimal("8.90")
assert credit.limit_of("c02") == Decimal("10000.00") and currency.convert(Decimal("11.683"), "GBP") == Decimal("10.00")

# The cache still prices each product once per group and step, and its
# answers never depend on what it priced before.
skus = sorted(catalog.PRODUCTS)
quantities = [1, 2, 3, 4, 5, 9, 10, 11, 12, 19, 20, 24, 25, 26, 49, 50, 99, 100, 101, 249, 250, 251, 9, 10, 3]
for sequence in (quantities, quantities[::-1]):
    for group in ("retail", "trade"):
        service = OrderService()
        seen = set()
        for sku in skus:
            bounds = catalog.FAMILIES[catalog.product(sku).family]["bounds"]
            for quantity in sequence:
                fresh = pricing.unit_price(sku, quantity, group)
                assert service.quotes.unit_price(sku, quantity, group) == fresh, (sku, quantity, group)
                seen.add((sku, expected_step(bounds, quantity)))
        assert service.quotes.misses == len(seen), (service.quotes.misses, len(seen))
        assert service.quotes.hits == len(skus) * len(sequence) - len(seen)

# Yesterday's replay matches in either order, and the export carries full region codes.
day = read_orders(os.path.join(TASK, "workspace", "tests", "data", "orders.csv"))
billed = read_billed(os.path.join(TASK, "workspace", "tests", "data", "billed.csv"))
assert replay(OrderService(), day, billed) == []
assert replay(OrderService(), day[::-1], billed) == []
export = export_bills(OrderService(), [Order(1, "c03", (Line("saw-500", 1),)), Order(2, "c05", (Line("saw-500", 1),))])
assert [row.split(",")[2] for row in export.splitlines()[1:]] == ["AT-9", "FR-IDF"], export
text = render_invoice(OrderService(), Order(7, "c02", (Line("hinge-80", 5), Line("washer-m6", 120))))
assert text.splitlines()[-2:] == ["  Tax 20.5%                                 5.92", "  Total (2 lines)                          34.82"], text
print("hidden tests passed")
