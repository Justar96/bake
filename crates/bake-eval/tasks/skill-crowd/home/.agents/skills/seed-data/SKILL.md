---
name: seed-data
description: "Maintain the seed data that fills a local or preview database with realistic fake records for development and demos - the seed files, the generator for fake names and amounts, the fixed demo accounts, and keeping seeds in step with the schema. Use when the local database is empty or broken after a reset, when a new table or column needs sample values for developers, when asked for demo data or fixtures for a preview environment, or when seeding fails. Seeds never contain real customer data and never run against production."
---

# Seed data

- Seeds live in `db/seeds/<NN>_<table>.sql` and run in file order after a reset.
- Generate fake values with a fixed random seed so every reset gives the same data.
- Keep the demo accounts (`demo@example.com`, `admin@example.com`) in `01_users.sql`.
