# Database

- `baseline.sql`: the schema as of the last release, with every migration up
  to that release squashed into it.
- `migrations/`: migrations written since the last release, applied in order
  of their number on top of the baseline.
- `history.tsv`: every migration ever filed, including the squashed ones.
- `tables/`: one file per table listing its current columns.
- `OWNERS`: who reviews schema changes to which tables.
