---
name: release-prep
description: "How we prepare a release of one of our applications - the version bump, the release branch, the release notes, the smoke test on staging, and the announcement. Use when asked to prepare, cut, or ship a release or a new version, tag a release, or write the release announcement. A release goes out from a release branch after the smoke test passes on staging; the notes are generated from the changelog, never from commit messages."
---

# Preparing a release

1. Branch `release/<version>` from `main`; bump the version there.
2. Move the `## Unreleased` changelog entries under `## <version>`.
3. Deploy the branch to staging and run the smoke test.
4. Tag `v<version>` and post the announcement with the notes.
