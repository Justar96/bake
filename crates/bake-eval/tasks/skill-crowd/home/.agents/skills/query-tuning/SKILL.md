---
name: query-tuning
description: "Make a slow database query fast - capture the query and its parameters, read EXPLAIN ANALYZE, spot sequential scans, bad row estimates, and sorts that spill to disk, and choose between rewriting the query, adding an index, or caching. Use when a query or endpoint is slow because of the database, when the slow query log or APM points at a statement, when asked whether an index would help, or when a query plan changed after a deploy. Produces a written analysis with the plan before and after; any index it recommends is then filed through the normal schema change process."
---

# Query tuning

1. Capture the exact statement and parameters from the slow query log.
2. Run `EXPLAIN (ANALYZE, BUFFERS)` on a production-sized copy.
3. Write `docs/perf/queries/<slug>.md` with the plan, the cause, and the proposed fix.
4. Hand any index to the schema change process; do not create it from here.
