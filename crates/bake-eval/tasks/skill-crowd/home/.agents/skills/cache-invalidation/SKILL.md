---
name: cache-invalidation
description: "Add caching to a slow read path or fix stale data served from a cache - choosing what to cache and for how long, cache keys that include every input, invalidating on write versus expiring by time, stampede protection, and measuring the hit rate. Use when an endpoint is slow because it recomputes the same result, when users see stale values after an update, when the cache hit rate drops after a deploy, or when asked about Redis, memcached, HTTP caching headers, CDN purges, ETags, or memoization. Every cache has a documented owner, key format, time to live, and the events that invalidate it, written next to the code that fills it, because a cache nobody can explain is a bug waiting to happen."
---

# Caching

- Keys are `<service>:<entity>:<id>:v<schema>`; bump `v` when the cached shape changes.
- Invalidate on write when staleness is visible to users; otherwise expire by time.
- Use a lock or request coalescing for keys that are expensive to rebuild.
