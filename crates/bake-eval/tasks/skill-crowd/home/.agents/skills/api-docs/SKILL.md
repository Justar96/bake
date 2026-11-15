---
name: api-docs
description: "How we write reference documentation for a public function, class, or HTTP endpoint - the sections, the example rules, and where the page goes. Use when asked to document an API, write reference docs, add docstrings that feed the docs site, or fix a page whose example no longer runs. Each public name gets one page with a one-sentence summary, parameters, return value, errors, and a runnable example."
---

# API reference pages

1. One page per public name, in `docs/api/<module>.<name>.md`.
2. Start with a one-sentence summary in the present tense.
3. Sections `Parameters`, `Returns`, `Raises`, `Example`, in that order.
4. Every example must run as written.
