from textkit import ws


def find_phrase(haystack, phrase):
    """The index of phrase in haystack, ignoring case and extra whitespace, or -1."""
    return ws.normalize_ws(haystack).lower().find(ws.normalize_ws(phrase).lower())
