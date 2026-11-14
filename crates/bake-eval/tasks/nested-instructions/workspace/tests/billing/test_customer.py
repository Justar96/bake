import unittest

from shop.app import dispatch


class CustomerTest(unittest.TestCase):
    def test_terms(self):
        status, body = dispatch("GET", "/customers/c001/terms", ("billing:read",))
        self.assertEqual(status, 200)
        self.assertIn(body["terms"], (14, 30, 60))


if __name__ == "__main__":
    unittest.main()
