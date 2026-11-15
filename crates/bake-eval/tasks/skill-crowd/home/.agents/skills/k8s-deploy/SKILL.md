---
name: k8s-deploy
description: "Deploy a service to our Kubernetes clusters or change how it runs there - manifests, resource requests and limits, probes, rollout strategy, horizontal autoscaling, and rolling back a bad deploy. Use when adding a new service to the cluster, changing replicas, CPU or memory, fixing CrashLoopBackOff or OOMKilled pods, adding a readiness probe, or when asked to deploy, roll out, scale, or roll back on Kubernetes. Triggers on - kubectl, Helm, manifest, Deployment, HPA, pod, probe, rollout, namespace."
---

# Kubernetes deploys

1. Manifests live in `deploy/<service>/`; change values there, never with `kubectl edit`.
2. Every container sets requests and limits and has readiness and liveness probes.
3. Roll back with `helm rollback <service>` and record the reason in the deploy channel.
