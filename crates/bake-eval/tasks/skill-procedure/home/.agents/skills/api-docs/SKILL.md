---
name: api-docs
description: How we write reference documentation for a public function, class, or HTTP endpoint - the sections, the example rules, and where the page goes. Use when asked to document an API.
---

# API reference pages

1. One page per public name, in `docs/api/<module>.<name>.md`.
2. Start with a one-sentence summary in the present tense ("Returns ...").
3. Then the sections `Parameters`, `Returns`, `Raises`, and `Example`, in
   that order; leave out a section that would be empty, except `Example`.
4. Every example must run as written: no ellipses, no undefined names.
5. Link the page from `docs/api/index.md`, keeping that list sorted.
