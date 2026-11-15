"""Numbers in reports: parsing input values and formatting results."""

from decimal import ROUND_HALF_EVEN, Decimal, InvalidOperation

from reportgen.errors import ReportError

CENT = Decimal("0.01")


def parse_number(value):
    """Parses a number such as "1,234.50", 7, or 2.5 as a Decimal."""
    if isinstance(value, bool):
        raise ReportError(f"not a number: {value!r}")
    if isinstance(value, float):
        value = repr(value)
    text = str(value).strip().replace(",", "")
    if not text:
        raise ReportError("empty value where a number is needed")
    try:
        number = Decimal(text)
    except InvalidOperation:
        raise ReportError(f"not a number: {value!r}") from None
    if not number.is_finite():
        raise ReportError(f"not a number: {value!r}")
    return number


def format_number(value):
    """A count as an integer, any other number with two decimals."""
    if isinstance(value, int):
        return str(value)
    return str(value.quantize(CENT, rounding=ROUND_HALF_EVEN))


def format_cell(value):
    """The text of one table cell."""
    if isinstance(value, (int, Decimal)) and not isinstance(value, bool):
        return format_number(value)
    return str(value)
