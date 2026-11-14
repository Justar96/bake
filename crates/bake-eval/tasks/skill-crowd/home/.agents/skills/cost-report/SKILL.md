---
name: cost-report
description: "Produce the monthly cloud cost report - pull the billing export, group spend by team and service, compare with the previous month and the budget, explain the largest changes, and flag idle or oversized resources. Use when asked for a cost report, a spend breakdown, why the cloud bill went up, which team or service costs the most, or where to save money on infrastructure. Works from the billing CSV export and the resource tags; untagged spend is reported as its own line, never spread across teams."
---

# Cost report

1. Load the billing export for the month and the one before.
2. Group by the `team` and `service` tags; untagged spend is its own row.
3. Explain every change above 10% or 500 USD, largest first.
4. Write `reports/costs/<YYYY-MM>.md`.
