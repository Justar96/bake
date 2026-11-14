import unittest

from textkit import find_phrase, normalize_ws, title_case
from textkit.filters import apply


class TextkitTest(unittest.TestCase):
    def test_normalize_ws(self):
        self.assertEqual(normalize_ws("  a \t b\n\nc "), "a b c")
        self.assertEqual(normalize_ws(""), "")

    def test_title_case(self):
        self.assertEqual(title_case("the  lord of the rings"), "The Lord of the Rings")

    def test_find_phrase(self):
        self.assertEqual(find_phrase("One  Two   Three", "two three"), 4)
        self.assertEqual(find_phrase("One Two", "four"), -1)

    def test_filters(self):
        self.assertEqual(apply("normalize_ws", " x  y "), "x y")
        self.assertEqual(apply("title", "a tale"), "A Tale")
        with self.assertRaises(ValueError):
            apply("missing", "x")


if __name__ == "__main__":
    unittest.main()
