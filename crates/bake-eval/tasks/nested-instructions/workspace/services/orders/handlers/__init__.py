"""The orders routes."""

from services.orders.handlers import order_lines, order_show, orders_list

ROUTES = [
    ("GET", "/orders", orders_list),
    ("GET", "/orders/{id}", order_show),
    ("GET", "/orders/{id}/lines", order_lines),
]
