"""Aggregators that need no arithmetic beyond addition."""

from decimal import Decimal



def total(numbers):
    return sum(numbers, Decimal(0))


def count(numbers):
    return len(numbers)


def minimum(numbers):
    return min(numbers)


def maximum(numbers):
    return max(numbers)
