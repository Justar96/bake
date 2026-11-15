---
name: spreadsheet-export
description: "Export data to a spreadsheet that people can actually use - CSV versus XLSX, column order and headers, number and date formats, frozen header rows, column widths, and avoiding the problems Excel causes with leading zeros, long numbers, and formulas injected through cell values. Use when asked to export, download, or generate a CSV or Excel file, when an export opens with mangled dates or numbers, or when building a report users will filter and sort."
---

# Spreadsheet exports

- Prefer XLSX for people, CSV for machines.
- Write dates as real dates and IDs as text so leading zeros survive.
- Prefix cells starting with `=`, `+`, `-`, or `@` with a single quote.
