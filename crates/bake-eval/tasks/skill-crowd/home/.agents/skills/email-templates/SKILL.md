---
name: email-templates
description: "Build or change a transactional email template - the shared layout, the plain-text part, the variables a template may use, previewing across clients, and the copy rules for subject lines. Use when adding a new email the product sends, changing the wording or design of an existing one, or fixing an email that renders badly in Outlook or Gmail. Every email has an HTML and a plain-text version, a subject under 50 characters, and no image that carries the only copy of important text."
---

# Email templates

- Templates live in `emails/<name>/` as `body.mjml`, `body.txt`, and `subject.txt`.
- Use only variables listed in `emails/<name>/vars.yaml`.
- Preview with `npm run emails:preview` in the Outlook and Gmail profiles before review.
