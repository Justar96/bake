---
name: graphql-schema
description: "Evolve our public GraphQL schema - adding types and fields, deprecating instead of removing, nullability rules, pagination with connections, naming, and checking a schema change against the operations clients actually run. Use when adding a query, mutation, type, or field to the GraphQL API, deprecating a field, changing nullability, fixing an N+1 problem in a resolver, or when the schema check in CI reports a breaking change. Triggers on - schema.graphql, SDL, resolver, @deprecated, connection, edges, cursor, DataLoader, persisted query, schema registry. This is the API schema that clients see, not the database: changes to tables and columns are handled elsewhere, and a new field here may be computed without any database change at all."
---

# GraphQL schema changes

- Never remove a field; mark it `@deprecated(reason: ...)` and wait for usage to reach zero.
- New fields are nullable unless the value can never be missing.
- Lists that can grow use connections with `first` and `after`.
- Run `npm run schema:check` against the registry before review.
