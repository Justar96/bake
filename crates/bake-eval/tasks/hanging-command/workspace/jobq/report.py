"""A one-line account of a queue's jobs."""


def describe(summary):
    """"3 done, 1 failed" from {"done": 3, "failed": 1}; "no jobs" when empty."""
    if not summary:
        return "no jobs"
    return ", ".join(f"{count} {state}" for state, count in summary.items())
