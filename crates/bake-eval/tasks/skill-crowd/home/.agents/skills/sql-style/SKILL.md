---
name: sql-style
description: "Format and write SQL in application code and reports the way our team reads it - uppercase keywords, one clause per line, explicit column lists instead of SELECT *, explicit JOIN syntax, meaningful table aliases, common table expressions instead of nested subqueries, and parameter placeholders instead of string formatting. Use when writing or reviewing a query in code, a reporting query, or a dbt model, when a query is hard to read, or when asked to format, lint, or clean up SQL. Covers sqlfluff configuration and the rules we turned off. It is about how queries read, not about what the schema contains or how fast a query runs, and it never changes what a query returns."
---

# SQL style

- Uppercase keywords; one clause per line; trailing commas in column lists.
- Explicit `JOIN ... ON`, never comma joins.
- Name CTEs after what they hold: `active_accounts`, not `t1`.
- Bind parameters; never format values into the string.
