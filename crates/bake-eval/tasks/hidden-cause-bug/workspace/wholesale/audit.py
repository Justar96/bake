"""A record of how each order was priced, for the accountants' spot checks."""


class AuditLog:
    """Keeps every entry; the service exports and clears it once a day."""

    def __init__(self):
        self.entries = []

    def record(self, order_number, what, amount):
        self.entries.append((order_number, what, amount))

    def for_order(self, order_number):
        return [(what, amount) for number, what, amount in self.entries if number == order_number]

    def clear(self):
        self.entries = []
