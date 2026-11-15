---
name: design-tokens
description: "Add or change design tokens - colors, spacing, radii, type scale, shadows, and motion durations - in the token source so every platform picks them up, instead of hard-coding values in components. Use when a component uses a raw hex color or pixel value, when adding a new brand color or theme, when dark mode colors are wrong, or when asked to update the design system, theme, or tokens. Triggers on - tokens.json, CSS variables, theme, palette, spacing scale, Figma variables, semantic color, dark mode."
---

# Design tokens

- Tokens live in `design/tokens.json`; components use semantic names (`color.text.muted`), never raw values.
- A new raw color needs a primitive token and a semantic alias.
- Run `npm run tokens` to regenerate the CSS variables and commit both.
