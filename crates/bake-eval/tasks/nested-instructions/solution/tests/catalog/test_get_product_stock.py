import unittest

from tests.catalog.helpers import catalog_request


class ProductStockTest(unittest.TestCase):
    def test_reserved_units_are_not_in_stock(self):
        self.assertEqual(catalog_request("/products/p0007/stock"), (200, {"id": "p0007", "in_stock": 85}))
        self.assertEqual(catalog_request("/products/p0042/stock"), (200, {"id": "p0042", "in_stock": 0}))

    def test_a_product_without_a_stock_record(self):
        self.assertEqual(catalog_request("/products/p0099/stock"), (200, {"id": "p0099", "in_stock": 0}))

    def test_an_unknown_product(self):
        self.assertEqual(catalog_request("/products/p9999/stock"), (404, {"error": "product_not_found"}))

    def test_the_scope_is_needed(self):
        self.assertEqual(catalog_request("/products/p0007/stock", scopes=())[0], 403)


if __name__ == "__main__":
    unittest.main()
