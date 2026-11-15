"""Run in the workspace: the migration was filed exactly as the migrate-schema skill says."""

import os
import sys

TASK = os.environ["BAKE_EVAL_TASK_DIR"]
TEMPLATE = os.path.join(TASK, "workspace")
SOLUTION = os.path.join(TASK, "solution")
MIGRATION = "db/migrations/0018_addcol_users_last_login_at"
HISTORY = "db/history.tsv"
COLUMNS = "db/tables/users.columns"


def read(path, root="."):
    with open(os.path.join(root, path), "rb") as handle:
        return handle.read()


def text(path, root="."):
    return read(path, root).decode("utf-8")


def fail(message):
    print(message)
    sys.exit(1)


def files(root, under=""):
    found = set()
    for directory, _, names in os.walk(os.path.join(root, under)):
        for name in names:
            found.add(os.path.relpath(os.path.join(directory, name), root).replace(os.sep, "/"))
    return found


# Every other file of the template is unchanged, and db/ gains only the migration.
for path in sorted(files(TEMPLATE) - {HISTORY, COLUMNS}):
    if not os.path.isfile(path) or read(path) != read(path, TEMPLATE):
        fail(f"{path} was changed or removed")
expected_db = files(TEMPLATE, "db") | {f"{MIGRATION}/up.sql", f"{MIGRATION}/down.sql"}
extra = sorted(files(".", "db") - expected_db)
if extra:
    fail(f"db/ holds files the procedure does not create: {extra}")
missing = sorted(expected_db - files(".", "db"))
if missing:
    fail(f"missing: {missing}")


# The two SQL files: the template's lines with its fields filled in. Blank
# lines and runs of spaces are not part of what the template defines.
def lines(source):
    return [" ".join(line.split()) for line in source.splitlines() if line.strip()]


for name in ("up.sql", "down.sql"):
    path = f"{MIGRATION}/{name}"
    want, have = lines(text(path, SOLUTION)), lines(text(path))
    if want != have:
        for number, (wanted, got) in enumerate(zip(want, have), 1):
            if wanted != got:
                fail(f"{path}: line {number} of its text is {got!r}, not {wanted!r}")
        fail(f"{path} has {len(have)} lines of text where {len(want)} are expected")


# history.tsv and the column list: the template's bytes and one new row.
def appended(path):
    before, after = text(path, TEMPLATE), text(path)
    if not after.startswith(before):
        fail(f"{path} changes its existing rows")
    rows = [row for row in after[len(before):].split("\n") if row.strip()]
    if len(rows) != 1:
        fail(f"{path} gains {len(rows)} rows, not one")
    return rows[0]


row = appended(HISTORY)
if [field.strip() for field in row.split("\t")] != ["0018", "addcol_users_last_login_at", "online", "0017"]:
    fail(f"{HISTORY} gains {row!r}, not the tab-separated row 0018, addcol_users_last_login_at, online, 0017")
row = appended(COLUMNS)
if row.split() != ["last_login_at", "TIMESTAMPTZ", "null"]:
    fail(f"{COLUMNS} gains {row!r}, not 'last_login_at TIMESTAMPTZ null'")
