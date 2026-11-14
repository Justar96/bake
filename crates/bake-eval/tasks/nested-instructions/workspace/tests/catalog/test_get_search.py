import unittest

from tests.catalog.helpers import catalog_request


class SearchTest(unittest.TestCase):
    def test_search(self):
        status, body = catalog_request("/search/marine")
        self.assertEqual(status, 200)
        self.assertTrue(body)


if __name__ == "__main__":
    unittest.main()
