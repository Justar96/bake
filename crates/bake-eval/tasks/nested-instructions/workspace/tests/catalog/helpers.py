"""Requests to the catalog as a reader of the catalog makes them."""

from shop.app import dispatch

READER = ("catalog:read",)


def catalog_request(path, scopes=READER):
    """(status, body) of GET `path`."""
    return dispatch("GET", path, scopes)
