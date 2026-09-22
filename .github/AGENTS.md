# Bake CI

Use Bun for installation and workspace commands. CI builds the Node agent runtime before running TUI checks and keyless PTY scenarios on Linux and macOS. The PTY driver requires POSIX terminals; do not add it to a Windows job without a Windows terminal implementation.

Keep workflow permissions read-only unless a requested operation requires more. Do not add upstream publishing, signing, deployment, or self-hosted-runner credentials. Release monitoring must report upstream changes for review, never merge or publish them automatically.
