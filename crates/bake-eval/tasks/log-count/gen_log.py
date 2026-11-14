"""Writes the task's log to standard output; the same bytes on every run."""

import sys

LINES = 6000
SERVICES = ("api", "auth", "billing", "search", "worker")
PATHS = ("/v1/items", "/v1/orders", "/v1/login", "/v1/search", "/v1/health")
CODES = ("E101", "E204", "E417", "E503")


class Lcg:
    """A fixed generator, so the log does not depend on the Python version."""

    def __init__(self, seed):
        self.state = seed

    def below(self, bound):
        self.state = (self.state * 6364136223846793005 + 1442695040888963407) % (1 << 64)
        return (self.state >> 33) % bound


def timestamp(second):
    minute, second = divmod(second, 60)
    hour, minute = divmod(minute, 60)
    day, hour = divmod(hour, 24)
    return f"2026-03-{1 + day:02d}T{hour:02d}:{minute:02d}:{second:02d}Z"


def lines():
    rng = Lcg(20260301)
    second = 8 * 3600
    for _ in range(LINES):
        second += 1 + rng.below(40)
        service = SERVICES[rng.below(len(SERVICES))]
        roll = rng.below(100)
        request = f"req={rng.below(1 << 24):06x}"
        if roll < 80:
            path = PATHS[rng.below(len(PATHS))]
            yield f"{timestamp(second)} INFO {service} handled {path} {request} ms={5 + rng.below(900)}"
        elif roll < 90:
            code = CODES[rng.below(len(CODES))]
            yield f"{timestamp(second)} WARN {service} retrying after {code} {request} attempt={1 + rng.below(3)}"
        else:
            code = CODES[rng.below(len(CODES))]
            yield f"{timestamp(second)} ERROR {service} request failed {request} code={code}"


def expected():
    """The count of ERROR lines with code=E417 and the first one's timestamp."""
    hits = [line.split(" ", 1)[0] for line in lines() if " ERROR " in line and line.endswith(" code=E417")]
    return len(hits), hits[0]


if __name__ == "__main__":
    sys.stdout.write("\n".join(lines()) + "\n")
