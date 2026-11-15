"""Writes the task's modules/ into the directory given, the same bytes on every run."""

import os
import sys

COUNT = 600
GROUPS = ("netcore", "storage", "billing", "ui", "reports", "search", "errorpages")
FEATURES = ("json", "http", "cache", "metrics")
USES = ("store.get", "store.put", "store.scan", "ledger.read", "ledger.list", "cache.hit", "cache.fill")


class Lcg:
    """A fixed generator, so the modules do not depend on the Python version."""

    def __init__(self, seed):
        self.state = seed

    def below(self, bound):
        self.state = (self.state * 6364136223846793005 + 1442695040888963407) % (1 << 64)
        return (self.state >> 33) % bound


def pick(rng, items, most):
    chosen = []
    for _ in range(1 + rng.below(most)):
        item = items[rng.below(len(items))]
        if item not in chosen:
            chosen.append(item)
    return sorted(chosen)


def modules():
    """(file name, text) of every module."""
    rng = Lcg(424242)
    for number in range(1, COUNT + 1):
        name = f"m{number:04d}"
        group = GROUPS[rng.below(len(GROUPS))]
        low, high = 2 + rng.below(4), 6 + rng.below(4)
        features = pick(rng, FEATURES, 3)
        uses = pick(rng, USES, 4)
        units = 8 + rng.below(90)
        if number > 290 and "store.get" in uses:
            # Only the older modules still read with store.get.
            uses = sorted((set(uses) - {"store.get"}) | {"store.scan"})
        if name == "m0183":
            low = 6
        if name == "m0412":
            low, high = 3, 6
        if name == "m0261":
            features = sorted(set(features) | {"tls"})
        if name == "m0334":
            uses = sorted(set(uses) | {"ledger.post_entry"})
        text = (
            f"name = {name}\n"
            f"group = {group}\n"
            f"api = {low}..{high}\n"
            f"features = {', '.join(features)}\n"
            f"uses = {', '.join(uses)}\n"
            f"units = {units}\n"
        )
        yield f"{name}.mod", text


def main():
    target = sys.argv[1]
    os.makedirs(target, exist_ok=True)
    for file_name, text in modules():
        with open(os.path.join(target, file_name), "w", encoding="utf-8", newline="\n") as handle:
            handle.write(text)


if __name__ == "__main__":
    main()
