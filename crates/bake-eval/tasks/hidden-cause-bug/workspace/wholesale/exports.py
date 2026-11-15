"""The accounting export: one CSV row per bill, as the ledger imports it."""

import csv
import io

from wholesale.customers import customer as find_customer
from wholesale.regions import normalize

COLUMNS = ("order", "customer", "region", "net", "promotion", "shipping", "tax_rate", "tax", "total")


def export_bills(service, orders):
    """The CSV text of the bills of `orders`, in their order."""
    out = io.StringIO()
    writer = csv.writer(out, lineterminator="\n")
    writer.writerow(COLUMNS)
    for order in orders:
        bill = service.bill(order)
        client = find_customer(order.customer)
        writer.writerow([
            order.number,
            client.id,
            normalize(client.region),
            f"{bill.subtotal:.2f}",
            f"{bill.promotion:.2f}",
            f"{bill.shipping:.2f}",
            f"{bill.tax_rate}",
            f"{bill.tax:.2f}",
            f"{bill.total:.2f}",
        ])
    return out.getvalue()
