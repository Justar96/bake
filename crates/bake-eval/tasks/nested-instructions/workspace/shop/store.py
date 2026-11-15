"""The nightly export in shop/data, loaded once."""

import functools
import json
import os

DATA = os.path.join(os.path.dirname(os.path.abspath(__file__)), "data")


@functools.lru_cache(maxsize=None)
def load(name):
    """The parsed shop/data/<name>.json."""
    with open(os.path.join(DATA, f"{name}.json"), encoding="utf-8") as handle:
        return json.load(handle)


def products():
    return load("products")


def stock():
    """Per product: units on the shelf, units reserved for open orders, and the bin."""
    return load("stock")


def orders():
    return load("orders")


def customers():
    return load("customers")
