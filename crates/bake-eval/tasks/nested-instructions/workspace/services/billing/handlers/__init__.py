"""The billing routes, in the order they were added."""

from services.billing.handlers import customer_handler, invoice_handler

ROUTES = [
    ("GET", "/invoices/{id}", invoice_handler.InvoiceHandler),
    ("GET", "/customers/{id}/terms", customer_handler.CustomerHandler),
]
