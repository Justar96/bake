# Billing handlers

1. Handlers are classes named `<Resource>Handler` in `handlers.py`-style
   modules named `<resource>_handler.py`.
2. Money is always a string with two decimals.
3. Every route is registered in `__init__.py` in the order it was added.
4. Tests for billing go in `tests/billing/`, one module per resource.
