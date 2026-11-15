---
name: pdf-reports
description: "Generate a PDF report from data - layout with a cover page, table of contents, charts, and tables that break across pages cleanly, using HTML and CSS print styles rendered to PDF. Use when asked to produce a PDF, export a report or invoice as PDF, fix page breaks, headers and footers, or fonts in a generated PDF, or turn a dashboard into a printable document. Covers page size and margins, embedding fonts, page numbers, and keeping table headers on every page."
---

# PDF reports

- Render HTML with print CSS; set `@page { size: A4; margin: 18mm; }`.
- Repeat table headers with `thead { display: table-header-group; }`.
- Embed fonts; never rely on system fonts in the renderer image.
