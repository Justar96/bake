import unittest
from decimal import Decimal

from wholesale.exports import export_bills
from wholesale.orders import Line, Order, OrderService
from wholesale.returns import ReturnError, credit_note


class ReturnsTest(unittest.TestCase):
    def test_a_return_is_refunded_at_the_price_paid(self):
        service = OrderService()
        bill = service.bill(Order(5, "c06", (Line("saw-500", 4), Line("glue-750", 2))))
        note = credit_note(5, bill, {"saw-500": 1})
        self.assertEqual((note.net, note.tax, note.total), (Decimal("18.33"), Decimal("3.67"), Decimal("22.00")))
        with self.assertRaises(ReturnError):
            credit_note(5, bill, {"saw-500": 5})


class ExportTest(unittest.TestCase):
    def test_the_export(self):
        text = export_bills(OrderService(), [Order(3, "c10", (Line("tape-50", 3),))])
        self.assertEqual(text, "order,customer,region,net,promotion,shipping,tax_rate,tax,total\n"
                               "3,c10,NL,6.30,0.00,4.90,21,2.35,13.55\n")


if __name__ == "__main__":
    unittest.main()
