import unittest
from decimal import Decimal

from wholesale import promotions
from wholesale.invoice import render_invoice
from wholesale.orders import Line, Order, OrderService
from wholesale.validation import OrderError


def order(number, customer, *lines, promotion=""):
    return Order(number, customer, tuple(Line(sku, quantity) for sku, quantity in lines), promotion)


class OrderTest(unittest.TestCase):
    def test_a_small_retail_order(self):
        bill = OrderService().bill(order(1, "c08", ("bolt-m6", 4), ("nut-m6", 4)))
        self.assertEqual((bill.subtotal, bill.shipping, bill.tax, bill.total),
                         (Decimal("2.20"), Decimal("4.90"), Decimal("1.35"), Decimal("8.45")))

    def test_each_line_is_rounded_half_up(self):
        # 0.05 less 5% is 0.0475 a washer: 11 cost 0.5225, 13 cost 0.6175.
        bill = OrderService().bill(order(1, "c10", ("washer-m6", 11), ("washer-m6", 13)))
        self.assertEqual([line.total for line in bill.lines], [Decimal("0.52"), Decimal("0.62")])

    def test_a_promotion(self):
        bill = OrderService().bill(order(1, "c01", ("saw-500", 2), promotion="spring10"))
        self.assertEqual((bill.subtotal, bill.promotion), (Decimal("35.91"), Decimal("3.59")))
        with self.assertRaises(promotions.PromotionError):
            OrderService().bill(order(2, "c08", ("saw-500", 2), promotion="SPRING10"))

    def test_bad_orders(self):
        with self.assertRaises(OrderError):
            OrderService().bill(order(1, "c01"))
        with self.assertRaises(OrderError):
            OrderService().bill(order(1, "c01", ("bolt-m6", 0)))

    def test_the_cache_prices_each_step_once(self):
        service = OrderService()
        service.total(order(1, "c01", ("hinge-80", 6), ("hinge-80", 12), ("hinge-80", 2)))
        self.assertEqual((service.quotes.misses, service.quotes.hits), (2, 1))

    def test_an_invoice(self):
        text = render_invoice(OrderService(), order(7, "c06", ("hinge-80", 3), ("washer-m6", 120)))
        self.assertEqual(
            text,
            "Invoice 7 for Brico Lyon\n"
            "  hinge-80           3 x 4.2000 =      12.60\n"
            "  washer-m6        120 x 0.0425 =       5.10\n"
            "  --------------------------------------------\n"
            "  Subtotal                                 17.70\n"
            "  Shipping                                  8.90\n"
            "  Tax 20%                                   5.32\n"
            "  Total (2 lines)                          31.92\n",
        )


if __name__ == "__main__":
    unittest.main()
