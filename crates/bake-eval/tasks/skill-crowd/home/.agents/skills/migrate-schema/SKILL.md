---
name: migrate-schema
description: "How we file a database schema migration in our services - the numbered migration directory, the up and down SQL files written from our template, the lock level and statements taken from the change-kinds table, the reviewer from db/OWNERS, the history row, and the table column list. Use when asked to add, write, create, or file a migration or any schema change - a new column, a dropped column, a new index, a renamed column - in a project with a db directory and a db/history.tsv file. Our migration runner rejects a migration whose files, header, or history row differ from this format, so follow it exactly even when the existing schema files suggest another layout. Not for changing values in existing rows (that is a backfill) or for upgrading an ORM or framework."
---

# Filing a schema migration

Work in the project's root. The migration runner reads these files and rejects
anything that differs from the formats below.

1. **Number.** The new migration's number is one more than the highest number
   in `db/history.tsv`, written with four digits (`0042`). That highest number
   is the new migration's parent.
2. **Kind.** Find the change in [change-kinds.csv](change-kinds.csv), in this
   skill's directory, by its `kind`. Use that row's `code`, `lock`, `forward`,
   and `rollback`.
3. **Slug.** `<code>_<table>_<column>`, in lowercase, for example
   `addidx_orders_placed_at`.
4. **Types.** Write column types in our canonical spelling: timestamps
   `TIMESTAMPTZ`, text `TEXT`, whole numbers `BIGINT`, true or false
   `BOOLEAN`, money `NUMERIC(19,4)`. Never `TIMESTAMP`, `VARCHAR`, or `INT`.
5. **Files.** Create the directory `db/migrations/<number>_<slug>/` holding
   exactly two files, `up.sql` and `down.sql`. Each is
   [migration-template.sql](migration-template.sql) with every `{field}`
   filled in: `{direction}` is `up` or `down`; `{reviewer}` is the table's
   reviewer in `db/OWNERS`; `{statement}` is the row's `forward` in `up.sql`
   and its `rollback` in `down.sql`, with `{table}`, `{column}`, `{type}`, and
   any other field filled in.
6. **History.** Append one tab-separated row to `db/history.tsv`:
   `<number>`, `<slug>`, `<lock>`, `<parent>`.
7. **Columns.** Keep `db/tables/<table>.columns` current: for a new column,
   append `<column> <TYPE> <null or not-null>` as its last line; for a dropped
   column, delete its line.

Do not edit `db/baseline.sql`: it is the squashed schema as of the last release,
and only the release tooling rewrites it. Code that uses a new column follows in
a separate change.
