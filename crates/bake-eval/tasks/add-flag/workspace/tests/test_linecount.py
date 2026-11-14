import contextlib
import io
import os
import tempfile
import unittest

import linecount


def run(argv):
    out = io.StringIO()
    with contextlib.redirect_stdout(out):
        status = linecount.main(["linecount.py", *argv])
    return status, out.getvalue()


class LinecountTest(unittest.TestCase):
    def setUp(self):
        self.dir = tempfile.TemporaryDirectory()
        self.addCleanup(self.dir.cleanup)

    def write(self, name, text):
        path = os.path.join(self.dir.name, name)
        with open(path, "w", encoding="utf-8") as handle:
            handle.write(text)
        return path

    def test_count_lines(self):
        self.assertEqual(linecount.count_lines(""), 0)
        self.assertEqual(linecount.count_lines("a\nb\n"), 2)
        self.assertEqual(linecount.count_lines("a\nb"), 2)

    def test_one_file(self):
        path = self.write("one.txt", "a b\nc\n")
        self.assertEqual(run([path]), (0, f"2 {path}\n"))

    def test_total_for_several_files(self):
        one = self.write("one.txt", "a b\nc\n")
        two = self.write("two.txt", "d")
        self.assertEqual(run([one, two]), (0, f"2 {one}\n1 {two}\n3 total\n"))


if __name__ == "__main__":
    unittest.main()
