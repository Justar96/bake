"""Lookups for the hot paths.

Grouping looks an aggregator up once per group and column. The lookups go
to the registry the report uses; a Registry's lookup is a dictionary access,
so nothing is cached across registries.
"""


def aggregator(registry, name):
    return registry.get_aggregator(name)


def formatter(registry, name):
    return registry.get_formatter(name)


def filter_operator(registry, name):
    return registry.get_filter(name)
