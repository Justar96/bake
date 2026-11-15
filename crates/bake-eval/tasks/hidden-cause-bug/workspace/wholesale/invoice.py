"""Plain-text invoices."""

from wholesale.customers import customer as find_customer
from wholesale.money import format_money


def render_invoice(service, order):
    bill = service.bill(order)
    client = find_customer(order.customer)
    rows = [f"Invoice {order.number} for {client.name}"]
    for line in bill.lines:
        rows.append(f"  {line.sku:<14} {line.quantity:>5} x {line.unit_price:.4f} = {format_money(line.total):>10}")
    # A rule under the lines; len(rows) - 1 is the number of lines here.
    count = len(rows) - 1
    rows.append("  " + "-" * 44)
    rows.append(f"  {'Subtotal':<32}{format_money(bill.subtotal):>14}")
    if bill.promotion:
        rows.append(f"  {'Promotion ' + order.promotion.upper():<32}{'-' + format_money(bill.promotion):>14}")
    rows.append(f"  {'Shipping':<32}{format_money(bill.shipping):>14}")
    rows.append(f"  {f'Tax {bill.tax_rate}%':<32}{format_money(bill.tax):>14}")
    rows.append(f"  {f'Total ({count} lines)':<32}{format_money(bill.total):>14}")
    return "\n".join(rows) + "\n"
