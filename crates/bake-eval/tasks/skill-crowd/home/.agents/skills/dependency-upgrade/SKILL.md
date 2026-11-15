---
name: dependency-upgrade
description: "Upgrade a third-party dependency safely - read the changelog between the two versions, list the breaking changes that affect our code, update the lockfile, run the full test suite, and record the upgrade. Use when asked to bump, update, or upgrade a package or library, when a security advisory names a dependency we use, when Dependabot or Renovate opens a pull request that fails, or when moving to a new major version. Covers npm, pip, and Cargo lockfiles; one dependency per change so a regression can be traced to it."
---

# Upgrading a dependency

1. Read every changelog entry between the current and the target version.
2. Search our code for each breaking change and fix the uses.
3. Update the manifest and lockfile only for this dependency.
4. Write `docs/deps/<package>-<version>.md`: why, breaking changes found, tests run.
