import unittest

from textkit import find_phrase, collapse_whitespace, title_case
from textkit.filters import apply


class TextkitTest(unittest.TestCase):
    def test_collapse_whitespace(self):
        self.assertEqual(collapse_whitespace("  a \t b\n\nc "), "a b c")
        self.assertEqual(collapse_whitespace(""), "")

    def test_title_case(self):
        self.assertEqual(title_case("the  lord of the rings"), "The Lord of the Rings")

    def test_find_phrase(self):
        self.assertEqual(find_phrase("One  Two   Three", "two three"), 4)
        self.assertEqual(find_phrase("One Two", "four"), -1)

    def test_filters(self):
        self.assertEqual(apply("collapse_whitespace", " x  y "), "x y")
        self.assertEqual(apply("title", "a tale"), "A Tale")
        with self.assertRaises(ValueError):
            apply("missing", "x")


if __name__ == "__main__":
    unittest.main()
