"""Writes logs/commits.log, newest commit first, the same bytes on every run."""

import sys

# Addresses and how often they commit; several belong to one person (see AUTHORS).
ADDRESSES = (
    ("ana@tidy.dev", 9), ("ana.lima@gmail.com", 3), ("bo@tidy.dev", 7), ("chris@tidy.dev", 5),
    ("dee@tidy.dev", 4), ("d.okafor@users.noreply.github.com", 4), ("eli@tidy.dev", 3),
    ("kim@example.org", 2), ("dependabot[bot]@users.noreply.github.com", 4), ("fay@tidy.dev", 1),
)
SUBJECTS = ("Fix wrapping of {x}", "Add tests for {x}", "Refactor {x}", "Document {x}", "Speed up {x}",
            "Handle empty {x}", "Tidy {x}", "Rename {x}")
THINGS = ("tabs", "long words", "the config loader", "blank lines", "the CLI", "--width", "unicode", "paragraphs")
RELEASES = ("2.0.0", "2.1.0", "2.2.0", "2.2.1")


class Lcg:
    """A fixed generator, so the log does not depend on the Python version."""

    def __init__(self, seed):
        self.state = seed

    def below(self, bound):
        self.state = (self.state * 6364136223846793005 + 1442695040888963407) % (1 << 64)
        return (self.state >> 33) % bound


def commits():
    """(sha, address, day, subject), oldest first."""
    rng = Lcg(2026)
    pool = [address for address, weight in ADDRESSES for _ in range(weight)]
    history = []
    day = 0
    for release in RELEASES + (None,):
        for _ in range(140 if release else 131):
            day += rng.below(2)
            address = pool[rng.below(len(pool))]
            if address.startswith("dependabot"):
                subject = f"Bump the dev group with {1 + rng.below(4)} updates"
            elif rng.below(9) == 0:
                subject = f"Merge pull request #{300 + len(history) % 200} from {address.split('@')[0]}/topic"
            else:
                subject = SUBJECTS[rng.below(len(SUBJECTS))].format(x=THINGS[rng.below(len(THINGS))])
            history.append((address, day, subject))
        if release:
            history.append(("release-bot@tidy.dev", day, f"Release {release}"))
    if all(address != "fay@tidy.dev" for address, _, _ in history[-131:]):
        history.insert(len(history) - 40, ("fay@tidy.dev", day - 20, "Fix a typo in the --help text"))
    out = []
    for number, (address, day, subject) in enumerate(history):
        sha = f"{(number * 2654435761 + 97) % (1 << 40):010x}"
        out.append((sha, address, day, subject))
    return out


def date(day):
    month, day_of_month = divmod(day, 28)
    year, month = divmod(month, 12)
    return f"{2024 + year}-{month + 1:02d}-{day_of_month + 1:02d}"


def main():
    lines = []
    for sha, address, day, subject in reversed(commits()):
        lines += [f"commit {sha}", f"Author: {address}", f"Date:   {date(day)}", "", f"    {subject}", ""]
    sys.stdout.write("\n".join(lines))


if __name__ == "__main__":
    main()
