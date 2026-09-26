---
description: "skill（技能）组地图：由提供方发现并经会话目录与 skill 工具加载的可复用 agent（智能体）指令，供浏览本组的用户与维护者阅读。"
kind: "package-group"
---

# skill/ — skill 能力家族

[English](README.md) | 中文

## 概述

skill 系列让 agent 在需要时发现并加载可复用的任务指令。`skill-filesystem` 从配置的目录发现指令，`skill-badge` 提供可选徽章，`tool-skill` 向模型提供目录与加载工具。

## 目录

- [包](#packages)
- [相关文档](#related-documentation)
- [开发备注](#dev-note)

-----

<a id="packages"></a>
## 包

| 包 | 职责 | ctx 键 |
|---|---|---|
| [`skill/`](skill/README.zh.md) | 合并任意提供方的 skill 目录、并按名称解析出胜出 skill 的注册表 | `ctx.skills` |
| [`skill-filesystem/`](skill-filesystem/README.zh.md) | 从项目、自定义与用户目录发现 skill，并监视其变更 | 注册到 `ctx.skills` |
| [`skill-badge/`](skill-badge/README.zh.md) | 随包附带官方「powered by dsh」徽章 skill，默认禁用 | 注册到 `ctx.skills` |
| [`tool-skill/`](tool-skill/README.zh.md) | 发布会话 skill 目录与面向模型的 `skill` 加载工具 | 注册到 `ctx.tools` |

-----

<a id="related-documentation"></a>
## 相关文档

先从子系统参考了解共享词汇，再阅读 Agent Note 了解设计依据。

- [skill 子系统参考](../../docs/subsystems/skills.zh.md)——注册表、提供方约定、本地发现优先级，以及目录与工具。
- [skill 调用策略 Agent Note](../../.agents/notes/implemented/feature/2026-07-28-skill-invocation-policy.zh.md)——模型与用户调用控制。

-----

<a id="dev-note"></a>
## 开发备注

<details>
<summary>维护者的工作上下文——点击展开</summary>

无。

</details>
