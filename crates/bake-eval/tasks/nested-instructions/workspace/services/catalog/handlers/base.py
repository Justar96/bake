"""What every catalog handler answers with."""


def respond(body, status=200):
    return status, body


def error(status, code):
    """A failure: `code` is a short snake_case reason such as "product_not_found"."""
    return status, {"error": code}
