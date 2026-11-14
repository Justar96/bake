---
name: load-testing
description: "Plan and run a load test against a service - pick the scenario and target rates from production traffic, ramp up in stages, watch latency percentiles and error rates, find the breaking point, and report what limited throughput. Use when asked to load test, stress test, or benchmark a service under traffic, check capacity before a launch, or find out how many requests per second an endpoint handles. Never runs against production; uses the staging environment with production-sized data."
---

# Load testing

1. Derive the scenario mix from a week of production access logs.
2. Ramp in steps of 25% of the target rate, five minutes each.
3. Stop at p99 latency above the SLO or errors above 1%.
4. Report the limit and the resource that hit it first.
