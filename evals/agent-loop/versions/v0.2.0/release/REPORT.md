# v0.2.0 agent-loop baseline

Candidate `v0.2.0`, measured on its own as a baseline.
Models: claude-sonnet-5-5, gemini-3.8-flash-medium, gpt-6.1-sol. Scenarios: duplicate_recovery, no_tools, ordinary_edit, path_discovery, stale_edit. Trials: 3. Started: unknown.

First recorded version. The samples come from the base arm of the 2026-09-30 run that measured 99d94e7; commit 5b63c1860f has the same tree as the v0.2.0 tag. The suite is the earlier five-case one (no_tools, ordinary_edit, path_discovery, stale_edit, duplicate_recovery), so compare later versions with it only through a paired run.

Token changes are the candidate's summed total over the base's, over pairs where both runs succeeded, with a paired bootstrap 95% interval.

## Regressions

Not assessed: a baseline record has nothing to compare against.

## `v0.2.0` on its own

| Model | Successes | First request bytes | Mean tokens per task | Mean requests per task | Cache-read share | Tool errors |
|---|---|---|---|---|---|---|
| claude-sonnet-5-5 | 15/15 | 23140 | 39333 | 4.42 | 97% | 15 |
| gemini-3.8-flash-medium | 15/15 | 22806 | 35601 | 5.92 | 1% | 12 |
| gpt-6.1-sol | 14/15 | 22782 | 33973 | 6.91 | 68% | 21 |

## Failed runs

| Model | Scenario | Trial | Version | Failure |
|---|---|---|---|---|
| gemini-3.8-flash-medium | stale_edit | 2 | v0.2.0+99d94e7 | empty final reply |
| gpt-6.1-sol | stale_edit | 1 | v0.2.0 | wall_clock_limit |
| gpt-6.1-sol | stale_edit | 1 | v0.2.0+99d94e7 | wall_clock_limit |
