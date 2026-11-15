import unittest

from inventory.stock import InsufficientStock, Stock


class StockTest(unittest.TestCase):
    def setUp(self):
        self.stock = Stock()
        self.stock.receive("bolt", 10)

    def test_available_counts_reservations(self):
        self.stock.reserve("bolt", 4)
        self.assertEqual(self.stock.available("bolt"), 6)

    def test_cannot_reserve_more_than_available(self):
        self.stock.reserve("bolt", 7)
        with self.assertRaises(InsufficientStock):
            self.stock.reserve("bolt", 4)
        self.assertEqual(self.stock.available("bolt"), 3)

    def test_release_returns_units(self):
        self.stock.reserve("bolt", 5)
        self.stock.release("bolt", 2)
        self.assertEqual(self.stock.available("bolt"), 7)

    def test_cannot_release_more_than_reserved(self):
        self.stock.reserve("bolt", 2)
        with self.assertRaises(ValueError):
            self.stock.release("bolt", 3)
        self.assertEqual(self.stock.reserved("bolt"), 2)

    def test_ship_leaves_shelf_and_reservation(self):
        self.stock.reserve("bolt", 6)
        self.stock.ship("bolt", 4)
        self.assertEqual(self.stock.on_hand("bolt"), 6)
        self.assertEqual(self.stock.reserved("bolt"), 2)
        self.assertEqual(self.stock.available("bolt"), 4)


if __name__ == "__main__":
    unittest.main()
