"""Prices an example order."""

from shop.cart import Cart
from shop.orders import place_order


def main():
    cart = Cart(customer_years=3)
    cart.add("widget", unit_price=4.5, quantity=14)
    cart.add("gasket", unit_price=1.25, quantity=30, clearance=True)
    cart.add("manual", unit_price=19.0, quantity=1)
    order = place_order(cart, region="north")
    for name, amount in order.summary():
        print(f"{name:>10}: {amount:8.2f}")


if __name__ == "__main__":
    main()
