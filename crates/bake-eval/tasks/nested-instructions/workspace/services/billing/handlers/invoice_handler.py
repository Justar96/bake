"""GET /invoices/{id}: the amount of an invoiced order."""

from decimal import Decimal

from shop import errors, store


class InvoiceHandler:
    REQUIRED_SCOPE = "billing:read"

    @staticmethod
    def handle(request):
        order = store.orders().get(request.params["id"])
        if order is None or order["status"] != "invoiced":
            return errors.not_found(f"no invoice {request.params['id']}")
        products = store.products()
        total = sum(Decimal(products[line["product"]]["price"]) * line["units"] for line in order["lines"])
        return 200, {"id": request.params["id"], "total": f"{total:.2f}"}
