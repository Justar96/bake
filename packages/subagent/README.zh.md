---
description: "subagent 包组：委派 seam、其进程内与进程外后端，以及面向模型的委派工具。"
kind: "package-group"
---

# subagent/：subagent 能力家族

[English](README.md) | 中文

## 概述

subagent 系列让 agent 将工作委派给进程内子级、继续子级工作并查询其状态。子级可以全新启动，也可以继承父级已完成的历史。面向模型的工具支持委派、后续消息、中断与列表查询。

## 目录

- [包](#packages)
- [相关文档](#related-documentation)
- [开发备注](#dev-note)

-----

<a id="packages"></a>
## 包

| 包 | 职责 | ctx 键 |
|---|---|---|
| [`subagent/`](subagent/README.zh.md) | 定义委派服务：提供方注册表、一次性运行、可继续子级与发现 | `ctx.subagents` |
| [`subagent-in-process-driver/`](subagent-in-process-driver/README.zh.md) | 提供共享的进程内运行驱动器 | 无 |
| [`subagent-spawn-in-process/`](subagent-spawn-in-process/README.zh.md) | 运行全新的进程内子 agent | 注册到 `ctx.subagents` |
| [`subagent-fork-in-process/`](subagent-fork-in-process/README.zh.md) | 运行从父级已完成历史派生的进程内子 agent | 注册到 `ctx.subagents` |
| [`tool-subagent/`](tool-subagent/README.zh.md) | 向模型公开委派 | 注册到 `ctx.tools` |
| [`tool-subagent-control/`](tool-subagent-control/README.zh.md) | 向模型提供向相邻 agent 发送消息、中断工作和列出子级状态的操作 | 注册到 `ctx.tools` |

-----

<a id="related-documentation"></a>
## 相关文档

- [Subagent 子系统](../../docs/subsystems/subagent.zh.md)——服务约定、提供方约定与终态结果语义。
- [Subagent 能力 seam](../../.agents/notes/implemented/feature/2026-06-21-subagent-capability-seam.zh.md)——委派能力家族的设计记录。
- [可继续的 subagent](../../.agents/notes/implemented/feature/2026-07-28-continuable-subagent-conversations.zh.md)——接受后续轮次的持久子级。
- [tool-subagent-control README](tool-subagent-control/README.zh.md)——后续消息、中断与列举接口。

<a id="dev-note"></a>
## 开发备注

无。
