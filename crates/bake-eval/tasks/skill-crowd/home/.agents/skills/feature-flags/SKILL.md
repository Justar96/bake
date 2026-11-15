---
name: feature-flags
description: "Add, roll out, and remove feature flags - naming, default values, targeting rules, percentage rollouts, and the cleanup that removes the flag and the dead branch once a feature is fully launched. Use when hiding unfinished work behind a flag, gradually releasing a feature, running an experiment, adding a kill switch, or when asked to clean up stale flags. A flag has an owner and a removal date from the day it is created."
---

# Feature flags

1. Name flags `<team>.<feature>`; register them in `flags.yaml` with an owner and a removal date.
2. Default to off in every environment.
3. Roll out 1%, 10%, 50%, 100%, watching errors at each step.
4. After 100% for two weeks, remove the flag and the old branch in one change.
