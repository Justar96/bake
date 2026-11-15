"""The built-in readers, aggregators, formatters, and filters."""

from reportgen.aggregators import basic, distinct, percentiles, stats
from reportgen.filters import compare, text_ops
from reportgen.formatters import csv_format, html, json_format, markdown, text
from reportgen.lazy import LAZY_READERS, lazy_reader
from reportgen.readers import csv_reader, jsonl_reader
from reportgen.registry import Registry


def default_registry():
    """A new Registry holding every built-in reader, aggregator, formatter, and filter."""
    registry = Registry()
    registry.add_reader(".csv", csv_reader.read_csv)
    registry.add_reader(".jsonl", jsonl_reader.read_jsonl)
    for suffix, (module, attribute) in LAZY_READERS.items():
        registry.add_reader(suffix, lazy_reader(module, attribute))
    for name, fn in (("sum", basic.total), ("count", basic.count), ("min", basic.minimum),
                     ("max", basic.maximum), ("mean", stats.mean), ("median", stats.median),
                     ("stdev", stats.population_stdev), ("distinct", distinct.count_distinct),
                     ("mode", distinct.mode), *percentiles.PERCENTILES.items()):
        registry.add_aggregator(name, fn)
    for name, fn in (("text", text.format_text), ("csv", csv_format.format_csv),
                     ("markdown", markdown.format_markdown), ("json", json_format.format_json),
                     ("html", html.format_html)):
        registry.add_formatter(name, fn)
    for name, fn in (("=", compare.equal), ("!=", compare.not_equal), ("<", compare.less),
                     (">", compare.greater), ("<=", compare.at_most), (">=", compare.at_least),
                     ("contains", text_ops.contains), ("startswith", text_ops.starts_with)):
        registry.add_filter(name, fn)
    return registry
