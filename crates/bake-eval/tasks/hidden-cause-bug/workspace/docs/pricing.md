# How an order is priced

This is the accountants' description of a bill, step by step. The code
follows it; where the two disagree, this page is right.

## Unit prices

1. Start from the product's base price (`wholesale/catalog.py`).
2. Take off the volume discount of the line's step on the family's price
   ladder. A ladder lists the lower bound of each step: with bounds
   `(1, 10, 50, 100)`, a line of 10 units is on the second step and gets
   its discount, and so does a line of 49; 50 units are on the third step.
3. For trade customers, take a further 5% off the result.

Unit prices are not rounded. The quote cache keeps one unit price per
product, customer group, and step, because every quantity on a step has the
same unit price.

## Lines and subtotal

Each line is the unit price times the quantity, rounded to the cent half up.
Two lines for the same product are priced separately. The subtotal is the
sum of the rounded lines.

## Promotions

A promotion code takes a percentage off the subtotal, rounded to the cent
half up. `SPRING10` is for trade customers only; `WELCOME5` is for
everyone.

## Shipping

Shipping depends on the destination country only, never the subdivision:
zone 1 is Germany, Austria, and the Netherlands, zone 2 Belgium and France.
The charge is that of the first weight band the parcel fits; above 20 kg,
every started kilogram costs 0.80 more. Orders from 500.00 after promotions
ship free.

## Tax

Tax is charged on the subtotal after promotions plus shipping, rounded to
the cent half up. The rate is the country's VAT plus the regional levy of
the customer's region where there is one:

| Region | Levy |
|---|---|
| DE-BY | 1.5% |
| AT-9 | 2% |
| FR-IDF | 0.5% |
| BE-BRU | 1% |

Region codes are matched after normalizing them: upper case, with `-`
between the country and the subdivision, so `de-by`, `DE_BY`, and
`de by` are all `DE-BY`.

## Total

The total is the subtotal, less the promotion, plus shipping and tax.
