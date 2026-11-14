from textkit.ws import collapse_whitespace

SMALL_WORDS = {"a", "an", "and", "in", "of", "the", "to"}


def title_case(text):
    """Capitalizes each word but the small ones, which stay lowercase unless first."""
    words = collapse_whitespace(text).lower().split(" ")
    titled = []
    for index, word in enumerate(words):
        if index > 0 and word in SMALL_WORDS:
            titled.append(word)
        else:
            titled.append(word[:1].upper() + word[1:])
    return " ".join(titled)
