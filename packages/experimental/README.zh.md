---
description: "实验组地图：可公开安装的预稳定原型。"
kind: "package-group"
---

# packages/experimental

[English](README.md) | 中文

## 概述

实验组目前包含 PTC 运行时的 CPython 后端。其约定可以变化，且不提供稳定性承诺；组外已发布产品不依赖它。

## 目录

- [包](#packages)
- [相关文档](#related-documentation)
- [开发备注](#dev-note)

-----

<a id="packages"></a>
## 包

| 包 | 职责 | ctx 键 |
|---|---|---|
| [`ptc-runtime-python`](ptc-runtime-python/README.zh.md) | PTC 执行 seam 的 CPython 子进程后端 | `ctx.ptcRuntime` |

-----

<a id="related-documentation"></a>
## 相关文档

- [实验包发布决策](../../.agents/notes/implemented/process/2026-09-12-experimental-publication-denylist.zh.md)——默认公开与私有例外。
- [实验子树规则](AGENTS.md)——实验状态放宽了什么、不放宽什么。

-----

<a id="dev-note"></a>
## 开发备注

<details>
<summary>维护者的工作上下文——点击展开</summary>

无。

</details>
