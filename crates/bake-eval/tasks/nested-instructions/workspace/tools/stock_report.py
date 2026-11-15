"""Prints the products whose shelf count is below a threshold: python3 tools/stock_report.py [N]."""

import json
import os
import sys

DATA = os.path.join(os.path.dirname(os.path.dirname(os.path.abspath(__file__))), "shop", "data")


def main(argv):
    threshold = int(argv[1]) if len(argv) > 1 else 10
    with open(os.path.join(DATA, "stock.json"), encoding="utf-8") as handle:
        stock = json.load(handle)
    low = sorted(product for product, record in stock.items() if record["shelf"] < threshold)
    print(f"{len(low)} products below {threshold} on the shelf")
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
