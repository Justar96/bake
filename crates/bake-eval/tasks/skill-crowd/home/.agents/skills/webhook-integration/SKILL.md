---
name: webhook-integration
description: "Add or change an outgoing or incoming webhook integration - payload versioning, signing and verifying requests with HMAC, retries with backoff, idempotency keys, ordering, and replaying failed deliveries. Use when integrating with a partner that sends or receives webhooks, adding a new webhook event, debugging deliveries that fail or arrive twice, or when asked about webhook signatures, retries, or a dead-letter queue."
---

# Webhooks

- Sign payloads with HMAC-SHA256 over the raw body; send the timestamp in the signature.
- Retry with exponential backoff up to 24 hours; then move to the dead-letter queue.
- Receivers dedupe on the `event_id`.
