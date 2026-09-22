# Bake documentation

The root README and CONTRIBUTING guide describe the supported Bake workflow. `tui/DESIGN.md` owns terminal behavior; package READMEs own shared runtime APIs. Upstream decision records and generated references describe their recorded subject, not a requirement to restore removed products.

Write current behavior, prerequisites, failure conditions, and commands that have been exercised. Keep paragraphs on one physical line. Update English and Chinese together when editing an existing pair; do not leave a stale pairing sidecar for rewritten pages. Bake does not run the upstream translation-pairing or documentation-publication system.

Preserve API obligations, security rules, and released persistence-format evidence. Do not edit archived Agent Notes or committed session generations. Prefer a link to the owning source over repeating API catalogs.

`bun run doc-sync` checks the Bake entry documents and terminal Markdown. Validate runtime documentation against its owning tests when changing an API. Do not claim that inherited reference documents have been comprehensively audited.
