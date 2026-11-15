"""Text filters, ignoring case."""

from reportgen.registry import filter_operator


@filter_operator("contains")
def contains(cell, operand):
    return operand.lower() in str(cell).lower()


@filter_operator("startswith")
def starts_with(cell, operand):
    return str(cell).lower().startswith(operand.lower())
