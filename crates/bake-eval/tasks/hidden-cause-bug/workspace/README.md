# wholesale

Prices, taxes, shipping, and invoices for a wholesale parts shop.

- Every product belongs to a family (`wholesale/catalog.py`), and each family
  has a price ladder: larger order lines get a volume discount by quantity
  step. Trade customers get a further group discount.
- Each order line is rounded to the cent on its own, half up, as the
  accountants require; promotions apply to the subtotal of the lines.
- Shipping is charged by the destination country and the parcel's weight.
- Tax is the country's VAT plus any regional levy, on the subtotal after
  promotions plus shipping.

The service prices many orders in one process and keeps a quote cache.

```sh
python3 -m unittest discover -s tests -t .
```
