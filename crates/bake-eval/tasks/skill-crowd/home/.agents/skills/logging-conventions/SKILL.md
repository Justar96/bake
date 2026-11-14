---
name: logging-conventions
description: "Add or change logging so logs stay searchable and safe - structured fields instead of formatted strings, the level to use for what, request and trace identifiers, and never logging secrets or personal data. Use when adding log lines, changing log levels, making logs easier to search, or when a log leaks a token, password, or email address. Covers our JSON log format, field names, sampling of noisy debug logs, and when an event should be a metric instead of a log line."
---

# Logging conventions

- Log events, not sentences: `log.info("invoice_sent", invoice_id=..., account_id=...)`.
- `error` means someone should look; expected failures are `warning` or `info`.
- Always include `request_id`; never log tokens, passwords, or full emails.
