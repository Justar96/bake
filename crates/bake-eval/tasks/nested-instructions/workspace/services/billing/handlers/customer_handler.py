"""GET /customers/{id}/terms: a customer's payment terms in days."""

from shop import errors, store


class CustomerHandler:
    REQUIRED_SCOPE = "billing:read"

    @staticmethod
    def handle(request):
        customer = store.customers().get(request.params["id"])
        if customer is None:
            return errors.not_found(f"no customer {request.params['id']}")
        return 200, {"id": request.params["id"], "terms": customer["terms"]}
