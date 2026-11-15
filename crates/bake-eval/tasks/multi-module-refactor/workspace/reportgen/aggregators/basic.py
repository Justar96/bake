"""Aggregators that need no arithmetic beyond addition."""

from decimal import Decimal

from reportgen.registry import aggregator


@aggregator("sum")
def total(numbers):
    return sum(numbers, Decimal(0))


@aggregator("count")
def count(numbers):
    return len(numbers)


@aggregator("min")
def minimum(numbers):
    return min(numbers)


@aggregator("max")
def maximum(numbers):
    return max(numbers)
