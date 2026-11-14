---
name: changelog-entry
description: "Write the user-facing changelog entry for a change - which section it goes in, how to phrase it for users rather than developers, how to credit contributors, and how to link the issue. Use when a pull request needs a changelog line, when asked to update CHANGELOG.md, or when release notes read like commit messages. Keeps entries short, in the past tense, and focused on what changed for the person using the product."
---

# Changelog entries

- Sections: Added, Changed, Fixed, Removed, under `## Unreleased`.
- One line per change, past tense, ending with the issue link: `Fixed ... (#123)`.
- Describe the effect on users, not the implementation.
