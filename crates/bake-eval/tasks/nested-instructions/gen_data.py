"""Writes shop/data/*.json and docs/products/*.md into the two directories
given, the same bytes on every run."""

import json
import os
import sys

CATEGORIES = ("fasteners", "hinges", "brackets", "tools", "adhesives", "chains", "springs",
              "seals", "fittings", "hooks", "rails", "wheels")
WORDS = ("steel", "brass", "zinc", "heavy", "light", "mini", "long", "short", "flat", "round",
         "black", "white", "coated", "marine", "quick", "twin")


class Lcg:
    """A fixed generator, so the data does not depend on the Python version."""

    def __init__(self, seed):
        self.state = seed

    def below(self, bound):
        self.state = (self.state * 6364136223846793005 + 1442695040888963407) % (1 << 64)
        return (self.state >> 33) % bound


def build():
    rng = Lcg(77)
    products = {}
    stock = {}
    for number in range(1, 1201):
        product = f"p{number:04d}"
        category = CATEGORIES[rng.below(len(CATEGORIES))]
        name = " ".join(WORDS[rng.below(len(WORDS))] for _ in range(2)) + " " + category[:-1]
        cents = 50 + rng.below(5000)
        products[product] = {"name": name, "category": category, "price": f"{cents // 100}.{cents % 100:02d}"}
        if rng.below(10):
            shelf = rng.below(400)
            reserved = rng.below(60) if rng.below(3) else 0
            stock[product] = {"shelf": shelf, "reserved": reserved, "bin": f"{chr(65 + rng.below(8))}-{rng.below(40):02d}"}
    stock["p0007"] = {"shelf": 120, "reserved": 35, "bin": "C-07"}
    stock["p0042"] = {"shelf": 10, "reserved": 25, "bin": "A-01"}
    stock.pop("p0099", None)
    orders = {}
    customers = {}
    for number in range(1, 301):
        customer = f"c{number:03d}"
        customers[customer] = {"name": f"Customer {number}", "terms": (14, 30, 60)[rng.below(3)]}
    for number in range(1, 901):
        lines = []
        for _ in range(1 + rng.below(4)):
            lines.append({"product": f"p{1 + rng.below(1200):04d}", "units": 1 + rng.below(50)})
        orders[f"o{number:05d}"] = {"customer": f"c{1 + rng.below(300):03d}", "lines": lines,
                                    "status": ("open", "packed", "shipped", "invoiced")[rng.below(4)]}
    return {"products.json": products, "stock.json": stock, "orders.json": orders, "customers.json": customers}


def write_docs(target, products):
    """One Markdown page a category, listing its products, as the docs site shows them."""
    os.makedirs(target, exist_ok=True)
    for category in CATEGORIES:
        rows = [f"| {key} | {value['name']} | {value['price']} |"
                for key, value in sorted(products.items()) if value["category"] == category]
        text = f"# {category.capitalize()}\n\n| Id | Name | Price |\n|---|---|---|\n" + "\n".join(rows) + "\n"
        with open(os.path.join(target, f"{category}.md"), "w", encoding="utf-8", newline="\n") as handle:
            handle.write(text)


def main():
    data, docs = sys.argv[1], sys.argv[2]
    os.makedirs(data, exist_ok=True)
    documents = build()
    for name, document in documents.items():
        with open(os.path.join(data, name), "w", encoding="utf-8", newline="\n") as handle:
            json.dump(document, handle, indent=2, sort_keys=True)
            handle.write("\n")
    write_docs(docs, documents["products.json"])


if __name__ == "__main__":
    main()
