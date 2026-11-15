"""Region codes: a country, optionally with a subdivision, as in "DE" or "DE-BY"."""


def normalize(code):
    """The canonical form of a region code: "DE" or "DE-BY". Accepts any case,
    surrounding spaces, and "_" or " " as the separator."""
    cleaned = code.strip().upper()
    for separator in ("_", " "):
        cleaned = cleaned.replace(separator, "-")
    country, _, subdivision = cleaned.partition("-")
    if len(country) != 2 or not country.isalpha():
        raise ValueError(f"not a region code: {code!r}")
    subdivision = subdivision.strip("-")
    if subdivision and len(subdivision) > 2:
        return f"{country}-{subdivision}"
    return country


def country_of(code):
    """The country of a region code: "DE" for "de-by"."""
    return normalize(code)[:2]
