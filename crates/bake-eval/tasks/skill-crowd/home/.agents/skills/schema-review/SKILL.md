---
name: schema-review
description: "Review a database schema migration that someone else has written, before it is merged - check locking and table rewrites on large tables, backward compatibility with the code that is already deployed, the rollback, index creation without blocking writes, and naming. Use when asked to review a migration, check whether a schema change is safe to deploy, or look over a pull request that touches the database schema. Read-only - it produces review comments on the existing migration and never writes or edits migration files itself; writing a new migration is a different process."
---

# Reviewing a schema migration

- Will it lock or rewrite a table with more than a million rows? Ask for an online variant.
- Can the currently deployed code run against both the old and the new schema?
- Does the rollback restore the previous schema exactly?
- Comment on the pull request; do not push changes to the migration.
