# textkit

Small text helpers.

- `normalize_ws(text)` collapses each run of whitespace to one space and trims both ends.
- `title_case(text)` capitalizes a title.
- `find_phrase(haystack, phrase)` finds a phrase, ignoring case and extra whitespace.

From the command line:

```sh
python3 cli.py --filter normalize_ws "  too   many   spaces "
python3 cli.py --filter title "the art of the deal"
```
