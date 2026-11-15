import unittest

from tests.catalog.helpers import catalog_request


class CategoriesTest(unittest.TestCase):
    def test_categories(self):
        status, body = catalog_request("/categories")
        self.assertEqual((status, len(body), body[0]), (200, 12, "adhesives"))

    def test_the_scope_is_needed(self):
        self.assertEqual(catalog_request("/categories", scopes=("orders:read",))[0], 403)


if __name__ == "__main__":
    unittest.main()
