---
name: ci-pipeline
description: "Change or debug our continuous integration pipeline - job layout, caching, required checks, flaky test quarantine, and how to make a slow pipeline faster without losing coverage. Use when a CI job fails for reasons unrelated to the change, when adding a new job or required check, when the build cache stops hitting, or when asked to speed up CI, retry a flaky job, or move a job between stages. Triggers on - GitHub Actions, workflow YAML, pipeline, runner, cache key, matrix build, required status check, flaky, quarantine."
---

# CI pipeline

1. Reproduce the failing job locally with the same image before changing the workflow.
2. Cache keys hash the lockfile; never key a cache on the branch name.
3. A test that fails twice in a week without a code change goes to `ci/quarantine.txt` with an owner.
