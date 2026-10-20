# The frozen 0.3 installers

`install.sh` and `install.ps1` here are `distribution/host/install.sh` and
`install.ps1` as `develop` carried them at `6e92fe6403`, the last commit before
those files became the [native installers](../../../../distribution/README.md#native-installers).
They are copies, byte for byte, of what `release/0.3` ships and are never
edited: a 0.3 fix lands on that branch.

`release:verify-local` installs a staged TypeScript release with these copies,
because the host's installers now install the Rust agent from the native
release route. 0.3 releases themselves ship from `release/0.3`, with its own
copy of the release guide and workflow.
