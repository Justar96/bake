import os
import unittest
from decimal import Decimal

from wholesale import catalog, margins, notifications, pricelist, reorder
from wholesale.credit import CreditHold
from wholesale.inventory import Inventory
from wholesale.orders import Line, Order, OrderService
from wholesale.replay import read_orders
from wholesale.reports import orders_by_region, revenue_by_family, top_products

DATA = os.path.join(os.path.dirname(os.path.abspath(__file__)), "data")


class ReportTest(unittest.TestCase):
    def test_top_products(self):
        orders = [Order(1, "c01", (Line("nut-m6", 30), Line("bolt-m6", 30))), Order(2, "c08", (Line("saw-500", 2),))]
        self.assertEqual(top_products(orders, 2), [("bolt-m6", 30), ("nut-m6", 30)])

    def test_revenue_by_family(self):
        orders = [Order(1, "c08", (Line("saw-500", 1), Line("tape-50", 1)))]
        self.assertEqual(revenue_by_family(OrderService(), orders),
                         {"consumables": Decimal("2.10"), "tools": Decimal("18.90")})

    def test_orders_by_country_level_region(self):
        orders = read_orders(os.path.join(DATA, "orders.csv"))
        regions = orders_by_region(orders)
        self.assertEqual(sum(regions.values()), 70)
        self.assertIn("NL", regions)


class PriceListTest(unittest.TestCase):
    def test_rows(self):
        rows = {sku: prices for _, sku, _, prices in pricelist.rows()}
        self.assertEqual(rows["saw-500"][0], ("1-2", Decimal("18.90")))
        self.assertTrue(pricelist.render("trade").startswith("Price list (trade)\n"))


class MarginTest(unittest.TestCase):
    def test_margins(self):
        bill = OrderService().bill(Order(1, "c08", (Line("saw-500", 2),)))
        self.assertEqual(margins.margins(bill, catalog.PRODUCTS), [("saw-500", Decimal("37.80"), Decimal("26.46"), Decimal("11.34"))])
        self.assertEqual(margins.margin_rate(bill, catalog.PRODUCTS), Decimal("0.3000"))
        self.assertEqual(margins.below_floor(bill, catalog.PRODUCTS, Decimal("0.31")), ["saw-500"])


class NotificationTest(unittest.TestCase):
    def test_messages(self):
        text = notifications.credit_hold_message(CreditHold("c08", Decimal("1600"), Decimal("1500")))
        self.assertIn("EUR 1,600.00", text)
        order = Order(4, "c06", (Line("saw-500", 1),))
        bill = OrderService().bill(order)
        self.assertIn("1 lines, 1 units", notifications.order_confirmation(order, bill, "GBP"))
        self.assertEqual(notifications.sales_alert(order, []), "")


class ReorderTest(unittest.TestCase):
    def test_suggestions(self):
        demand = reorder.weekly_demand([Order(1, "c01", (Line("bolt-m6", 40),))], weeks=2)
        stock = Inventory({"bolt-m6": 30})
        self.assertEqual(reorder.suggestions(stock, demand, cover_weeks=4, pack_sizes={"bolt-m6": 25}), [("bolt-m6", 50)])


if __name__ == "__main__":
    unittest.main()
