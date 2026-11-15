import unittest

from tests.catalog.helpers import catalog_request


class HealthTest(unittest.TestCase):
    def test_health_is_public(self):
        self.assertEqual(catalog_request("/health", scopes=()), (200, {"status": "ok"}))


if __name__ == "__main__":
    unittest.main()
