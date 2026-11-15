"""The tidying itself."""

import textwrap


def tidy_text(text, width=79):
    """Wraps paragraphs to `width`, trims trailing spaces, and leaves one blank
    line between paragraphs and one newline at the end."""
    paragraphs = [block for block in text.replace("\t", "    ").split("\n\n") if block.strip()]
    wrapped = [textwrap.fill(" ".join(block.split()), width) for block in paragraphs]
    return "\n\n".join(wrapped) + "\n" if wrapped else ""
