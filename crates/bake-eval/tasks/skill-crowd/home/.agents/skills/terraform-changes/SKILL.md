---
name: terraform-changes
description: "Change infrastructure through Terraform safely - module layout, remote state, reading the plan before applying, avoiding resource replacement, importing existing resources, and moving resources between modules without destroying them. Use when adding or changing cloud resources, when a plan wants to destroy and recreate something, when state drifted from reality, or when asked about terraform plan, apply, import, state mv, modules, or workspaces. Every apply is preceded by a reviewed plan."
---

# Terraform changes

1. Run `terraform plan -out plan.bin` and read every `-/+` line.
2. Use `moved` blocks instead of destroying and recreating.
3. Apply only a reviewed plan file, from CI, never from a laptop.
