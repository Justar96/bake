import unittest

from shop.app import dispatch


class InvoiceTest(unittest.TestCase):
    def test_an_unknown_invoice(self):
        self.assertEqual(dispatch("GET", "/invoices/o99999", ("billing:read",))[0], 404)


if __name__ == "__main__":
    unittest.main()
