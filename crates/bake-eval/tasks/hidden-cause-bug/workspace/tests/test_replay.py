import os
import unittest

from wholesale.orders import OrderService
from wholesale.replay import read_billed, read_orders, replay

DATA = os.path.join(os.path.dirname(os.path.abspath(__file__)), "data")


class ReplayTest(unittest.TestCase):
    """Replays yesterday's orders through one service, as production runs
    them, against the totals the old billing system charged."""

    def test_yesterday_matches_what_was_billed(self):
        orders = read_orders(os.path.join(DATA, "orders.csv"))
        billed = read_billed(os.path.join(DATA, "billed.csv"))
        self.assertEqual(replay(OrderService(), orders, billed), [])


if __name__ == "__main__":
    unittest.main()
