"""Percentile aggregators, by the nearest-rank method."""

import math

from reportgen.registry import aggregator


def nearest_rank(numbers, percent):
    ordered = sorted(numbers)
    rank = max(1, math.ceil(percent * len(ordered) / 100))
    return ordered[rank - 1]


for _percent in (25, 75, 90):
    aggregator(f"p{_percent}")(lambda numbers, percent=_percent: nearest_rank(numbers, percent))
