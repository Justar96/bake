from textkit import ws


def find_phrase(haystack, phrase):
    """The index of phrase in haystack, ignoring case and extra whitespace, or -1."""
    return ws.collapse_whitespace(haystack).lower().find(ws.collapse_whitespace(phrase).lower())
