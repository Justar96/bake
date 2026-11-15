---
name: perf-profiling
description: "Find out why code is slow before changing it - reproduce with a benchmark, profile CPU and memory, read flame graphs, separate waiting on I/O from computing, and confirm the fix with the same benchmark. Use when a function, request, page, or job is slow, when CPU or memory usage is too high, or when asked to optimize, speed up, or profile something. Not for slow database queries specifically, which have their own query-tuning steps; this covers application code."
---

# Profiling

1. Write a benchmark that reproduces the slowness; record the baseline.
2. Profile (py-spy, perf, or the browser profiler) and read the flame graph.
3. Change one thing; re-run the same benchmark and report both numbers.
