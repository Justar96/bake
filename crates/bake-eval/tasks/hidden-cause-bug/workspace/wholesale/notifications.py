"""Messages to customers and to the sales team."""

from wholesale.currency import show
from wholesale.customers import customer as find_customer
from wholesale.money import format_money


def credit_hold_message(hold):
    client = find_customer(hold.customer_id)
    return (
        f"Dear {client.name},\n\n"
        f"your order would bring your balance to EUR {format_money(hold.owed)}, above your credit limit "
        f"of EUR {format_money(hold.limit)}. We have held it until a payment arrives.\n"
    )


def order_confirmation(order, bill, currency="EUR"):
    client = find_customer(order.customer)
    total = f"EUR {format_money(bill.total)}"
    if currency != "EUR":
        total += f" (about {show(bill.total, currency)})"
    units = sum(line.quantity for line in bill.lines)
    return (
        f"Dear {client.name},\n\n"
        f"thank you for order {order.number}: {len(bill.lines)} lines, {units} units, {total}.\n"
    )


def sales_alert(order, skus):
    """A note to the sales team about lines sold below the margin floor."""
    if not skus:
        return ""
    return f"Order {order.number}: low margin on {', '.join(sorted(set(skus)))}\n"
