---
name: data-backfill
description: "How we write and run a one-off data backfill, also called a data migration - a script that rewrites or fills in values in existing rows, run in batches with a dry run, progress logging, and a way to resume. Use when existing records need new values computed, a column added earlier must be filled in for old rows, bad data must be corrected in bulk, or rows must be copied between tables. Not for changing the shape of the database itself - new tables, columns, or indexes are schema changes and follow their own process; this skill only moves or rewrites data inside the schema that exists. Scripts live in scripts/backfills and are reviewed like code."
---

# Data backfills

1. Write `scripts/backfills/<YYYYMMDD>_<what>.py` with a `--dry-run` flag that is on by default.
2. Process rows in batches of 1,000 ordered by primary key; log the last key after each batch.
3. Accept `--resume-from <key>` so an interrupted run continues.
4. Record the run (who, when, rows changed) in the pull request.
