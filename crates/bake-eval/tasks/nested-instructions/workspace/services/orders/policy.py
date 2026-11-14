"""Who may see which orders."""


def may_see(scopes, order):
    return "orders:read" in scopes or "orders:admin" in scopes
