import unittest

from tests.catalog.helpers import catalog_request


class ProductTest(unittest.TestCase):
    def test_a_product(self):
        status, body = catalog_request("/products/p0007")
        self.assertEqual(status, 200)
        self.assertEqual(sorted(body), ["category", "id", "name", "price"])
        self.assertEqual(body["id"], "p0007")

    def test_an_unknown_product(self):
        self.assertEqual(catalog_request("/products/p9999"), (404, {"error": "product_not_found"}))


if __name__ == "__main__":
    unittest.main()
