import unittest

from shop.app import dispatch

READER = ("orders:read",)


class OrdersTest(unittest.TestCase):
    def test_open_orders(self):
        status, body = dispatch("GET", "/orders", READER)
        self.assertEqual(status, 200)
        self.assertTrue(body)

    def test_an_order_and_its_lines(self):
        self.assertEqual(dispatch("GET", "/orders/o00001", READER)[0], 200)
        self.assertEqual(dispatch("GET", "/orders/o00001/lines", READER)[0], 200)
        self.assertEqual(dispatch("GET", "/orders/o99999", READER), (404, {"message": "no order o99999"}))


if __name__ == "__main__":
    unittest.main()
