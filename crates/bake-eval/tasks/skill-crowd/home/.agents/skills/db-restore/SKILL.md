---
name: db-restore
description: "Restore a database, a table, or a set of rows from backups - choosing between the nightly snapshot and point-in-time recovery, restoring into a scratch instance first, verifying row counts, and copying back only what was lost. Use when data was deleted or corrupted by mistake, when a migration or deploy went wrong and data must be recovered, when asked to roll a database back to a point in time, or to test that backups can actually be restored. Never restores over the live database directly; every restore starts in a scratch instance."
---

# Restoring from backup

1. Find the last good time from logs; prefer point-in-time recovery for losses under 7 days old.
2. Restore into a scratch instance named `restore-<ticket>`.
3. Compare row counts and spot-check rows against the report of what was lost.
4. Copy back only the affected rows, in a transaction, with a second person watching.
