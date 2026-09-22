# Workspace scripts

Bun runs Bake's workspace scripts. Prefer package scripts and maintained tools over custom orchestration. Keep Node where a check exercises the Node agent or its module resolver.

A generator must support a read-only check and reject missing workspace inputs. Test both retained and removed dependency paths. Source-analysis checks resolve packages to source; built-profile smokes deliberately use `lib/`.

Scripts own their temporary paths and child processes. Restore process-global state and await teardown on failure. Never rewrite recorded session generations to satisfy a check.
