"""Comparisons: numbers when both sides are numbers, else text."""

from reportgen.errors import ReportError
from reportgen.registry import filter_operator
from reportgen.values import parse_number


def _pair(cell, operand):
    try:
        return parse_number(cell), parse_number(operand)
    except ReportError:
        return str(cell), operand


@filter_operator("=")
def equal(cell, operand):
    left, right = _pair(cell, operand)
    return left == right


@filter_operator("!=")
def not_equal(cell, operand):
    return not equal(cell, operand)


@filter_operator("<")
def less(cell, operand):
    left, right = _pair(cell, operand)
    return type(left) is type(right) and left < right


@filter_operator(">")
def greater(cell, operand):
    left, right = _pair(cell, operand)
    return type(left) is type(right) and left > right


@filter_operator("<=")
def at_most(cell, operand):
    return less(cell, operand) or equal(cell, operand)


@filter_operator(">=")
def at_least(cell, operand):
    return greater(cell, operand) or equal(cell, operand)
