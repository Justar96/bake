"""Credit limits: how much a customer may owe before new orders are held."""

from decimal import Decimal

from wholesale.customers import customer as find_customer

# The most a customer of each group may owe, open orders included.
LIMITS = {"retail": Decimal("1500.00"), "trade": Decimal("10000.00")}

# Customers the credit team has given a limit of their own.
OVERRIDES = {"c04": Decimal("4000.00"), "c11": Decimal("25000.00")}


class CreditHold(Exception):
    """An order that would take a customer over their limit."""

    def __init__(self, customer_id, owed, limit):
        super().__init__(f"{customer_id} would owe {owed}, over the limit of {limit}")
        self.customer_id = customer_id
        self.owed = owed
        self.limit = limit


def limit_of(customer_id):
    if customer_id in OVERRIDES:
        return OVERRIDES[customer_id]
    return LIMITS[find_customer(customer_id).group]


def check(customer_id, balance, new_total):
    """Raises CreditHold if `new_total` on top of `balance` goes over the
    customer's limit. Reaching the limit exactly is allowed."""
    owed = balance + new_total
    limit = limit_of(customer_id)
    if owed > limit:
        raise CreditHold(customer_id, owed, limit)
    return limit - owed


def headroom(customer_id, balance):
    """How much more the customer may order; never below zero."""
    return max(limit_of(customer_id) - balance, Decimal("0.00"))
