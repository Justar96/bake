"""Cached lookups for the hot paths.

Grouping looks an aggregator up once per group and column, and a report
may have thousands of groups, so the lookups that grouping and rendering
make go through these caches.
"""

import functools

from reportgen import registry


@functools.lru_cache(maxsize=256)
def aggregator(name):
    return registry.get_aggregator(name)


@functools.lru_cache(maxsize=64)
def formatter(name):
    return registry.get_formatter(name)


@functools.lru_cache(maxsize=64)
def filter_operator(name):
    return registry.get_filter(name)


def clear():
    """Forgets every cached lookup, as a plugin that replaces one needs."""
    aggregator.cache_clear()
    formatter.cache_clear()
    filter_operator.cache_clear()
