---
name: release-prep
description: How we prepare a release of one of our projects - the version bump, the codename, the release notes file and its exact format, the contributors, the pending changes, and the release log. Use when asked to prepare, cut, or ship a release or a new version.
---

# Preparing a release

Work in the project's root. Do every step; the release tooling reads these
files and rejects anything that differs from the formats below. This
procedure replaces the RELEASING.md some of our repositories still carry
from the 1.x days: where the two differ, follow this one.

## 1. Version

Write the new version, `MAJOR.MINOR.PATCH`, as the only line of `VERSION`,
and set `__version__` in the package's `__init__.py` to the same string.
The version `VERSION` held before is the previous version.

## 2. Codename

A patch release (only PATCH changes) keeps the codename of the release it
patches. Any other release takes the first name in `CODENAMES.txt`, in file
order, that no row of `RELEASES.csv` uses yet.

## 3. Commits and contributors

`logs/commits.log` is the project's commit log, newest first. The commits
of this release are those above (newer than) the newest commit whose
subject is `Release <version>`. Of these, leave out merge commits (the
subject starts with `Merge `) and commits by bots (an address containing
`[bot]`, and `release-bot@tidy.dev`).

- **Commits** is the number of commits left.
- **Contributors** counts those commits by person. `AUTHORS` maps addresses
  to names, and one person may have several addresses; an address that is
  not in `AUTHORS` stands for itself. List them as `Name (count)`, by count
  from most to fewest, then by name, separated by `, `.

## 4. Release notes

Collect the change lines in `UNRELEASED.txt`: every line that is not blank
and does not start with `#`, written `<type> #<issue number> <text>`. The
types that go into the notes are `security`, `removed`, `added`, and
`fixed`; any other type (`internal`, `docs`, ...) is left out of the notes.

Write `release-notes/v<version>-<codename>.md`, with the codename in
lowercase in the file name, for example `release-notes/v1.4.0-amber.md`;
create the directory if it is missing. Its content is
[notes-template.md](notes-template.md), from this skill's directory, with
every `{field}` filled in:

- `{codename}` is capitalized; `{changes}` counts the entries in the
  sections.
- `{sections}` is the sections, in this order: Security, Removed, Added,
  Fixed. Each is a `## <Name>` line followed by its entries, and one blank
  line separates two sections. Leave out a section with no entries.
- In a section, entries are sorted by issue number as numbers (71 before
  102), and each is `- <text> (#<issue number>)`.
- The file ends with a single newline.

For example:

```
## Security
- Tokens are no longer written to the debug log. (#88)

## Fixed
- Fixed the exit status when no file matches. (#71)
- Fixed a crash on empty input. (#102)
```

## 5. Pending changes

Remove every change line you collected from `UNRELEASED.txt`, including the
ones whose type is not listed in the notes, and the blank lines with them,
so that only its `#` comment lines remain.

## 6. Release log

Append a row to the end of `RELEASES.csv`:
`<version>,<codename in lowercase>,<change lines removed in step 5>,<commits>`.
