"""Customer statements: the bills of a period, payments, and the balance."""

from dataclasses import dataclass, field
from decimal import Decimal

from wholesale.customers import customer as find_customer
from wholesale.money import ZERO, format_money

# Days a customer of each group has to pay a bill.
TERMS = {"retail": 14, "trade": 30}


@dataclass
class Statement:
    customer_id: str
    opening: Decimal = ZERO
    entries: list = field(default_factory=list)

    def bill(self, number, day, amount):
        self.entries.append((day, f"Invoice {number}", amount))

    def payment(self, reference, day, amount):
        self.entries.append((day, f"Payment {reference}", -amount))

    def balance(self):
        return self.opening + sum((amount for _, _, amount in self.entries), ZERO)

    def overdue(self, today):
        """Invoices older than the customer's terms that the payments made
        since, applied oldest first, have not covered."""
        terms = TERMS[find_customer(self.customer_id).group]
        paid = sum((-amount for _, what, amount in self.entries if amount < 0), ZERO) - self.opening
        late = []
        for day, what, amount in sorted(entry for entry in self.entries if entry[2] > 0):
            if paid >= amount:
                paid -= amount
                continue
            remaining = amount - max(paid, ZERO)
            paid = ZERO
            if today - day > terms:
                late.append((what, remaining))
        return late

    def render(self):
        client = find_customer(self.customer_id)
        rows = [f"Statement for {client.name}", f"  {'Opening balance':<30}{format_money(self.opening):>12}"]
        running = self.opening
        for day, what, amount in sorted(self.entries):
            running += amount
            rows.append(f"  day {day:>3}  {what:<20}{format_money(amount):>12}{format_money(running):>12}")
        rows.append(f"  {'Balance':<30}{format_money(self.balance()):>12}")
        return "\n".join(rows) + "\n"
