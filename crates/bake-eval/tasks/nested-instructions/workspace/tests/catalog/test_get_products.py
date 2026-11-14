import unittest

from tests.catalog.helpers import catalog_request


class ProductsTest(unittest.TestCase):
    def test_the_first_page(self):
        status, body = catalog_request("/products")
        self.assertEqual((status, body["count"], len(body["ids"]), body["ids"][0]), (200, 1200, 20, "p0001"))


if __name__ == "__main__":
    unittest.main()
