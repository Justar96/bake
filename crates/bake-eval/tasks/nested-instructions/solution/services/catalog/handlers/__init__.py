"""The catalog's routes, sorted by path."""

from services.catalog.handlers import (
    get_categories,
    get_category_products,
    get_health,
    get_product,
    get_product_stock,
    get_products,
    get_search,
)

ROUTES = [
    ("GET", "/categories", get_categories),
    ("GET", "/categories/{id}/products", get_category_products),
    ("GET", "/health", get_health),
    ("GET", "/products", get_products),
    ("GET", "/products/{id}", get_product),
    ("GET", "/products/{id}/stock", get_product_stock),
    ("GET", "/search/{term}", get_search),
]
