# Shop service

- Run the tests with `python3 -m unittest discover -s tests -t .` before handing work back.
- Error responses carry a JSON body `{"message": "<text for people>"}`; build them with the helpers in `shop/errors.py`.
- Read data through `shop/store.py`; do not open the JSON files yourself.
- Keep modules small: one concern a module.
