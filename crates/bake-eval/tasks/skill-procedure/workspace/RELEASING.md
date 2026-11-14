# Releasing tidy

1. Bump the version in `VERSION`.
2. Write `docs/releases/<version>.md`: a `# tidy <version>` heading, a
   "What's new" list with every line of `UNRELEASED.txt`, and a "Thanks to"
   line naming everyone in `git shortlog -s` since the last tag.
3. Add the version as the first row of `RELEASES.csv`.
4. Tag the release and push the tag.
