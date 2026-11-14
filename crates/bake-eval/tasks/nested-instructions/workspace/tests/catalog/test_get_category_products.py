import unittest

from tests.catalog.helpers import catalog_request


class CategoryProductsTest(unittest.TestCase):
    def test_a_category(self):
        status, body = catalog_request("/categories/hinges/products")
        self.assertEqual(status, 200)
        self.assertTrue(body and all(product_id.startswith("p") for product_id in body))

    def test_an_unknown_category(self):
        self.assertEqual(catalog_request("/categories/nope/products"), (404, {"error": "category_not_found"}))


if __name__ == "__main__":
    unittest.main()
