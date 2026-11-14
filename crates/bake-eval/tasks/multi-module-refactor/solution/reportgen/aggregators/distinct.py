"""Aggregators over the distinct values."""



def count_distinct(numbers):
    return len(set(numbers))


def mode(numbers):
    """The most frequent number; the smallest of those tied."""
    counts = {}
    for number in numbers:
        counts[number] = counts.get(number, 0) + 1
    best = max(counts.values())
    return min(number for number, seen in counts.items() if seen == best)
