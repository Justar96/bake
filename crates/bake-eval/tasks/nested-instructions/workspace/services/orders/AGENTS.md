# Orders service

These rules apply to everything under `services/orders/`.

1. Handler modules are named `<resource>_<action>.py`: `order_show.py`,
   `orders_list.py`.
2. Every handler needs the scope `orders:read` and checks the customer with
   `policy.may_see()` first.
3. Errors use `shop/errors.py`, as the root AGENTS.md says.
4. All the orders tests live in `tests/orders/test_orders.py`.
