"""Percentile aggregators, by the nearest-rank method."""

import functools
import math


def nearest_rank(numbers, percent):
    ordered = sorted(numbers)
    rank = max(1, math.ceil(percent * len(ordered) / 100))
    return ordered[rank - 1]


PERCENTILES = {f"p{percent}": functools.partial(nearest_rank, percent=percent) for percent in (25, 75, 90)}
