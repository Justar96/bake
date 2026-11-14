---
name: accessibility-audit
description: "Audit a web page or component for accessibility problems and write up what to fix, ordered by impact. Covers keyboard navigation and focus order, visible focus rings, color contrast against WCAG 2.2 AA, form labels and error messages, alt text, landmark regions, heading order, reduced motion, and how a screen reader announces dynamic content. Use when asked to check accessibility, a11y, WCAG compliance, screen reader support, or keyboard-only use, or when a page fails an automated axe or Lighthouse check. Read-only - it reports findings with the element, the rule, and a suggested fix, it does not edit code."
---

# Accessibility audit

1. Tab through the page; note every element that cannot be reached or shows no focus.
2. Check text contrast with the computed colors (4.5:1 body text, 3:1 large text).
3. List findings as `severity | element | rule | fix`, highest severity first.
