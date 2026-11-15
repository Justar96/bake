---
name: analytics-events
description: "How we name, document, and add product analytics events so dashboards keep working - the object_action naming rule, required properties, the tracking plan file, and privacy review for anything that could identify a person. Use when adding a new tracked event, renaming an event, adding properties to an existing event, or when someone asks why an event is missing from a dashboard. Triggers on - track(), analytics, tracking plan, event schema, Segment, Amplitude, funnel, conversion event, product metrics."
---

# Analytics events

- Name events `object_action` in snake_case: `invoice_sent`, never `SentInvoice`.
- Every event carries `account_id`, `surface`, and `app_version`.
- Add the event to `analytics/tracking-plan.yaml` before shipping the code that sends it.
- Free-text or email properties need a privacy review first.
