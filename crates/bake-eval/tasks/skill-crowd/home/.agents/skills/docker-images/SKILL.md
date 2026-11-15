---
name: docker-images
description: "Write or fix a Dockerfile and the image it builds - small multi-stage builds, pinned base images, non-root users, layer ordering for cache hits, health checks, and scanning the result. Use when creating a Dockerfile, when an image is too large or builds slowly, when a container runs as root, when a security scanner flags the base image, or when asked to containerize a service. Covers .dockerignore, build arguments versus runtime environment, and the labels our registry requires."
---

# Docker images

1. Multi-stage: build in a full image, copy the artifact into a slim runtime image.
2. Pin base images by digest.
3. Copy the lockfile and install dependencies before copying the source.
4. Run as a numeric non-root user; add a `HEALTHCHECK`.
