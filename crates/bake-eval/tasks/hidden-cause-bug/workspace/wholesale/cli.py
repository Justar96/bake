"""python3 -m wholesale.cli COMMAND ORDERS.csv [ARG]

Commands:
  invoice ORDERS.csv NUMBER   print the invoice of one order
  label ORDERS.csv NUMBER     print the shipping label of one order
  export ORDERS.csv           print the accounting export of every order
  replay ORDERS.csv BILLED.csv
                              list the orders whose total differs from what was billed
"""

import sys

from wholesale.exports import export_bills
from wholesale.invoice import render_invoice
from wholesale.labels import label
from wholesale.orders import OrderService
from wholesale.replay import read_billed, read_orders, replay


def main(argv=None):
    args = list(sys.argv[1:] if argv is None else argv)
    if len(args) < 2 or args[0] not in ("invoice", "label", "export", "replay"):
        print(__doc__, file=sys.stderr)
        return 2
    command, path, rest = args[0], args[1], args[2:]
    orders = read_orders(path)
    service = OrderService()
    if command == "export":
        sys.stdout.write(export_bills(service, orders))
        return 0
    if command == "replay":
        wrong = replay(service, orders, read_billed(rest[0]))
        for line in wrong:
            print(line)
        return 1 if wrong else 0
    wanted = [order for order in orders if str(order.number) == rest[0]]
    if not wanted:
        print(f"no order {rest[0]} in {path}", file=sys.stderr)
        return 1
    # Price the orders before it too, as the service would have that day.
    for order in orders:
        if order is wanted[0]:
            break
        service.total(order)
    sys.stdout.write(render_invoice(service, wanted[0]) if command == "invoice" else label(wanted[0]))
    return 0


if __name__ == "__main__":
    sys.exit(main())
