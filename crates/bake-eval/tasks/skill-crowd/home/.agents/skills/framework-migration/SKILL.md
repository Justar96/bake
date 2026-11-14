---
name: framework-migration
description: "Plan and carry out a migration from one framework or major library version to another across a codebase - such as moving from SQLAlchemy 1.4 to 2.0, from Express to Fastify, or from a class-based to a hooks-based UI - in small steps that keep the application working. Use when asked to migrate the project to a new framework, port code to a new major version, replace a library everywhere, or plan a large codemod. Produces a migration plan with ordered steps, compatibility shims, and the point where the old code is deleted. Not for database schema changes or data fixes."
---

# Framework migrations

1. Inventory every use of the old framework (`rg` for imports) and group them by module.
2. Add a compatibility layer so old and new code can run side by side.
3. Migrate one module per change, with tests passing after each.
4. Track progress in `docs/migrations/<from>-to-<to>.md`; delete the shim last.
